#![cfg(test)]

use super::*;
use aeris_tastytrade_market_adapter::SessionWindow;
#[test]
fn resolved_instrument_cache_reuses_identity_and_clears_on_authorization_change() {
    let api = BrokerApi::default();
    let item = SearchInstrument {
        symbol: "SPY".into(),
        instrument_type: "Equity".into(),
        exchange: Some("NYSE".into()),
        description: None,
    };
    let key = (item.instrument_type.clone(), item.symbol.clone());
    let resolved = ResolvedInstrument {
        symbol: item.symbol.clone(),
        streamer_symbol: item.symbol.clone(),
        venue: "NYSE".into(),
        instrument_type: item.instrument_type.clone(),
        tick_size: None,
        tick_sizes: Vec::new(),
        point_value: None,
        currency: None,
        expiration_date: None,
        first_notice_date: None,
        last_trade_date: None,
    };
    {
        let mut state = api.state.lock().unwrap();
        state
            .instruments
            .insert(key.clone(), (Instant::now(), Ok(resolved)));
        state.instrument_order.push_back(key);
    }
    let stop = Arc::new(AtomicBool::new(false));
    assert_eq!(api.instrument(&item, &stop).unwrap().symbol, "SPY");
    assert_eq!(api.state.lock().unwrap().instrument_order.len(), 1);
    api.state.lock().unwrap().instruments.insert(
        (item.instrument_type.clone(), item.symbol.clone()),
        (Instant::now(), Err("Provider lookup unavailable".into())),
    );
    assert_eq!(
        api.instrument(&item, &stop).unwrap_err(),
        "Provider lookup unavailable"
    );
    *api.futures_sessions.lock().unwrap() = Some((
        Instant::now(),
        [
            session_at(100, 200),
            MarketSession {
                collection: MarketCollection::Cfe,
                ..session_at(100, 200)
            },
        ],
    ));
    *api.equity_session.lock().unwrap() = Some((
        Instant::now(),
        MarketSession {
            collection: MarketCollection::Equity,
            ..session_at(100, 200)
        },
    ));
    let mut equity = instrument();
    equity.instrument_id = "tastytrade:Equity:SPY".into();
    equity.venue_id = "NYSE".into();
    assert_eq!(
        api.market_session(&equity).unwrap().collection,
        MarketCollection::Equity
    );
    equity.instrument_id = "tastytrade:Equity Option:SPY  261016C00600000".into();
    assert!(api.market_session(&equity).is_none());
    equity.instrument_id = "tastytrade:Equity:SPY".into();
    api.clear().unwrap();
    assert!(api.state.lock().unwrap().instruments.is_empty());
    assert!(api.market_session(&instrument()).is_none());
    assert!(api.market_session(&equity).is_none());
}

#[test]
fn provider_calendar_status_uses_the_cached_dated_window_and_fails_closed() {
    let api = BrokerApi::default();
    assert_eq!(
        api.market_session_status("tastytrade:Future:/ESZ6", "/ESZ26:XCME", 150)
            .phase,
        MarketSessionPhase::Unknown
    );
    *api.futures_sessions.lock().unwrap() = Some((
        Instant::now(),
        [
            session_at(100, 200),
            MarketSession {
                collection: MarketCollection::Cfe,
                ..session_at(100, 200)
            },
        ],
    ));
    assert_eq!(
        api.market_session_status("tastytrade:Future:/ESZ6", "/ESZ26:XCME", 150)
            .phase,
        MarketSessionPhase::Regular
    );
    for (symbol, streamer) in [
        ("/MESZ6", "/MESZ26:XCME"),
        ("/RTYZ6", "/RTYZ26:XCME"),
        ("/CLX6", "/CLX26:XNYM"),
        ("/GCZ6", "/GCZ26:XCEC"),
        ("/ZNZ6", "/ZNZ26:XCBT"),
    ] {
        assert_eq!(
            api.market_session_status(&format!("tastytrade:Future:{symbol}"), streamer, 150)
                .phase,
            MarketSessionPhase::Regular,
            "{streamer} must use the provider CME collection",
        );
    }
    assert_eq!(
        api.market_session_status("tastytrade:Future:/ESZ6", "/ESZ26:XCME", 200)
            .phase,
        MarketSessionPhase::Closed
    );
    assert_eq!(
        api.market_session_status("tastytrade:Future:/ESZ6", "ESZ26", 150)
            .phase,
        MarketSessionPhase::Unknown
    );
    *api.equity_session.lock().unwrap() = Some((
        Instant::now(),
        MarketSession {
            collection: MarketCollection::Equity,
            ..session_at(100, 200)
        },
    ));
    assert_eq!(
        api.market_session_status("tastytrade:Equity:AAPL", "AAPL", 150)
            .phase,
        MarketSessionPhase::Regular
    );
    assert_eq!(
        api.market_session_status("tastytrade:Equity:AAPL", "AAPL", 200)
            .phase,
        MarketSessionPhase::Closed
    );
    api.equity_session.lock().unwrap().as_mut().unwrap().0 = Instant::now()
        .checked_sub(Duration::from_mins(7))
        .expect("test instant has seven minutes of history");
    assert_eq!(
        api.market_session_status("tastytrade:Equity:AAPL", "AAPL", 150)
            .phase,
        MarketSessionPhase::Unknown
    );
}

#[test]
fn equity_query_with_loaded_futures_catalog_returns_preliminary_empty_result() {
    let search = SearchProviderInstruments {
        consumer_id: 1,
        search_generation: 1,
        startup_resolve: false,
        provider: "tastytrade".into(),
        query: "SPY".into(),
        maximum_results: 100,
        categories: aeris_contracts::InstrumentSearchCategories::ALL,
    };
    let event = search_catalog(
        &search,
        &mut vec![serde_json::from_str(
            r#"{"symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME","product-code":"ES","expiration-date":"2026-12-18","active":true,"active-month":true,"notional-multiplier":"50.0","tick-size":"0.25"}"#,
        )
        .unwrap()],
        &mut BTreeMap::new(),
        &BrokerApi::default(),
        &Arc::new(AtomicBool::new(false)),
        &Arc::new(AtomicU64::new(1)),
    )
    .unwrap();
    let CatalogEvent::Search(result) = event else {
        panic!("equity queries need a preliminary result before remote search");
    };
    assert_eq!(result.instruments, [] as [ProviderInstrumentSummary; 0]);
}

#[test]
fn tastytrade_futures_display_symbols_drop_only_the_leading_slash() {
    assert_eq!(tastytrade_display_symbol("/ESZ6", "Future"), "ESZ6");
    assert_eq!(tastytrade_display_symbol("AAPL", "Equity"), "AAPL");
}

#[test]
fn futures_search_accepts_slashless_and_slash_prefixed_queries() {
    let future: FutureInstrument = serde_json::from_str(
        r#"{"symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME","product-code":"ES","expiration-date":"2026-12-18","active":true,"active-month":true,"notional-multiplier":"50.0","tick-size":"0.25"}"#,
    )
    .unwrap();
    for query in ["ES", "ESZ6", "/ES"] {
        let event = search_catalog(
            &SearchProviderInstruments {
                consumer_id: 1,
                search_generation: 1,
                startup_resolve: false,
                provider: "tastytrade".into(),
                query: query.into(),
                maximum_results: 10,
                categories: aeris_contracts::InstrumentSearchCategories::ALL,
            },
            &mut vec![future.clone()],
            &mut BTreeMap::new(),
            &BrokerApi::default(),
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicU64::new(1)),
        )
        .unwrap();
        let CatalogEvent::Search(result) = event else {
            panic!("futures search should complete locally");
        };
        assert_eq!(result.instruments[0].display_symbol, "ESZ6");
    }
}

#[test]
fn excluded_futures_category_returns_no_catalog_futures() {
    let future: FutureInstrument = serde_json::from_str(
        r#"{"symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME","product-code":"ES","expiration-date":"2026-12-18","active":true,"active-month":true,"notional-multiplier":"50.0","tick-size":"0.25"}"#,
    )
    .unwrap();
    let event = search_catalog(
        &SearchProviderInstruments {
            consumer_id: 1,
            search_generation: 1,
            startup_resolve: false,
            provider: "tastytrade".into(),
            query: "/ES".into(),
            maximum_results: 10,
            categories: aeris_contracts::InstrumentSearchCategories {
                futures: false,
                equities: true,
            },
        },
        &mut vec![future],
        &mut BTreeMap::new(),
        &BrokerApi::default(),
        &Arc::new(AtomicBool::new(false)),
        &Arc::new(AtomicU64::new(1)),
    )
    .unwrap();
    let CatalogEvent::Search(result) = event else {
        panic!("catalog search completes locally");
    };
    assert_eq!(result.instruments, [] as [ProviderInstrumentSummary; 0]);
}

#[test]
fn no_match_query_returns_empty_search_result() {
    let search = SearchProviderInstruments {
        consumer_id: 1,
        search_generation: 1,
        startup_resolve: false,
        provider: "tastytrade".into(),
        query: "/NO_SUCH_FUTURE".into(),
        maximum_results: 100,
        categories: aeris_contracts::InstrumentSearchCategories::ALL,
    };
    let event = search_catalog(
        &search,
        &mut vec![serde_json::from_str(
            r#"{"symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME","product-code":"ES","expiration-date":"2026-12-18","active":true,"active-month":true,"notional-multiplier":"50.0","tick-size":"0.25"}"#,
        )
        .unwrap()],
        &mut BTreeMap::new(),
        &BrokerApi::default(),
        &Arc::new(AtomicBool::new(false)),
        &Arc::new(AtomicU64::new(1)),
    )
    .unwrap();
    let CatalogEvent::Search(result) = event else {
        panic!("a no-match query must return an empty search result");
    };
    assert_eq!(result.instruments, [] as [ProviderInstrumentSummary; 0]);
}

#[test]
fn equity_install_uses_the_upper_band_when_price_is_unknown() {
    let resolved = ResolvedInstrument {
        symbol: "AAPL".into(),
        streamer_symbol: "AAPL".into(),
        venue: "NASDAQ".into(),
        instrument_type: "Equity".into(),
        tick_size: Some(1_000_000),
        tick_sizes: vec![
            aeris_tastytrade_market_adapter::PriceIncrementBand {
                value: 10_000,
                threshold: Some(100_000_000),
            },
            aeris_tastytrade_market_adapter::PriceIncrementBand {
                value: 1_000_000,
                threshold: None,
            },
        ],
        point_value: None,
        currency: Some("USD".into()),
        expiration_date: None,
        first_notice_date: None,
        last_trade_date: None,
    };
    let installed = install_resolved(resolved, 1, 1);
    assert_eq!(installed.price_increment, Some(1_000_000));
    let terms = installed.contract_metadata.expect("equity contract terms");
    assert_eq!(terms.point_value, Some(1));
    assert_eq!(terms.point_value_scale, Some(0));
    assert_eq!(terms.currency.as_deref(), Some("USD"));
    assert_eq!(terms.contract_expiry, None);
}

#[test]
fn future_install_preserves_provider_multiplier_tick_and_available_currency() {
    let future: FutureInstrument = serde_json::from_str(
        r#"{"symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME","product-code":"ES","expiration-date":"2026-12-18","active":true,"active-month":true,"notional-multiplier":"50.0","tick-size":"0.25"}"#,
    )
    .expect("sanitized future");
    let installed = install_resolved(
        ResolvedInstrument::from_future(&future).expect("provider terms"),
        1,
        1,
    );
    assert_eq!(installed.price_increment, Some(25_000_000));
    let terms = installed.contract_metadata.expect("future contract terms");
    assert_eq!(terms.point_value, Some(5_000_000_000));
    assert_eq!(terms.point_value_scale, Some(DATA_SCALE));
    assert_eq!(terms.currency.as_deref(), Some("USD"));
}

#[test]
fn daily_candles_request_session_days_not_fixed_24_hours() {
    assert_eq!(
        PRESENTATION.daily_bars,
        aeris_contracts::ProviderDailyBarCadence::TradingSession
    );
    assert_eq!(
        candle_period(BarPeriod::session(1).unwrap()).as_deref(),
        Ok("d")
    );
    assert_eq!(
        candle_period(BarPeriod::time(43_200).unwrap()).as_deref(),
        Ok("12h")
    );
    assert!(candle_period(BarPeriod::time(86_400).unwrap()).is_err());
}

#[test]
fn second_candles_use_dxfeed_second_periods() {
    assert_eq!(
        candle_period(BarPeriod::time(1).unwrap()).as_deref(),
        Ok("s")
    );
    assert_eq!(
        candle_period(BarPeriod::time(5).unwrap()).as_deref(),
        Ok("5s")
    );
    assert_eq!(
        candle_period(BarPeriod::time(30).unwrap()).as_deref(),
        Ok("30s")
    );
    assert_eq!(
        candle_period(BarPeriod::time(60).unwrap()).as_deref(),
        Ok("m")
    );
    assert!(PRESENTATION.chart_interval_labels.contains(&"1s"));
}

#[test]
fn late_candle_subscription_keeps_its_first_forming_candle_start() {
    let minute = BarPeriod::time(60).unwrap();
    let mut starts = BTreeMap::new();
    let start = |starts: &mut BTreeMap<String, i64>, symbol: &str, now| {
        candle_start_ms(starts, symbol.into(), minute, now).unwrap()
    };
    assert_eq!(start(&mut starts, "ES{=m}", 120_000), 60_001);
    assert_eq!(
        start(&mut starts, "ES{=m}", 300_000),
        60_001,
        "a reconcile keeps the subscribed start instead of re-subscribing"
    );
    assert_eq!(start(&mut starts, "NQ{=m}", 300_000), 240_001);
}

/// dxFeed delivers only candles whose open time is at or after `fromTime`, so a
/// subscription starting at the demand instant never receives the candle that
/// is already forming and leaves it stale until the next period opens.
#[test]
fn live_candle_subscription_includes_the_forming_candle() {
    const MINUTE_MS: i64 = 60_000;
    let five_minutes = BarPeriod::time(300).unwrap();
    let forming_open = 1_767_225_600_000;
    for elapsed in [0, 3 * MINUTE_MS + 20_000, 5 * MINUTE_MS - 1] {
        let now = forming_open + elapsed;
        let mut starts = BTreeMap::new();
        let from =
            candle_start_ms(&mut starts, "/NQZ26:XCME{=5m}".into(), five_minutes, now).unwrap();
        assert!(
            from <= forming_open,
            "fromTime {from} must not pass the forming candle opened at {forming_open}"
        );
        assert!(
            from > forming_open - 5 * MINUTE_MS,
            "fromTime {from} must not replay the completed candle before it"
        );
    }
}

#[test]
fn calendar_candle_subscription_includes_the_forming_session_week_and_month() {
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    // Monday 2026-03-09 15:00 UTC. The session day and the week opened with
    // the Sunday 23:00 UTC Globex open, a three-session candle with the
    // Wednesday evening open, and the month on March 1.
    let now = 1_773_068_400_000;
    for (period, forming_open) in [
        (BarPeriod::session(1).unwrap(), now - 16 * HOUR),
        (BarPeriod::session(3).unwrap(), now - 4 * DAY - 16 * HOUR),
        (BarPeriod::week(1).unwrap(), now - 16 * HOUR),
        (BarPeriod::month(1).unwrap(), now - 8 * DAY - 15 * HOUR),
    ] {
        let mut starts = BTreeMap::new();
        let from = candle_start_ms(&mut starts, "/ESH26:XCME".into(), period, now).unwrap();
        assert!(
            from <= forming_open,
            "{period:?} fromTime {from} must not pass the forming candle at {forming_open}"
        );
    }
    let mut starts = BTreeMap::new();
    assert!(
        candle_start_ms(
            &mut starts,
            "/ESH26:XCME".into(),
            BarPeriod::tick(10).unwrap(),
            now
        )
        .is_err()
    );
}
fn instrument() -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "tastytrade".into(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: "tastytrade:Future:/ESZ6".into(),
        provider_symbol: "/ESZ26:XCME".into(),
        display_symbol: "/ESZ6".into(),
        venue_id: "CME".into(),
        price_scale: 8,
        quantity_scale: 8,
        entitlement_id: ENTITLEMENT.into(),
        price_increment: Some(25_000_000),
        contract_metadata: None,
    }
}
fn request() -> HistoryRequest {
    let instrument = instrument();
    let series = BarSeriesKey {
        provider_id: instrument.provider.clone(),
        instrument_id: instrument.instrument_id.clone(),
        entitlement_id: ENTITLEMENT.into(),
        period: BarPeriod::time(60).unwrap(),
        definition_version: 1,
    };
    HistoryRequest {
        ticket: crate::market_service::fixture_history_ticket(&series),
        series,
        provider_generation: ProviderGeneration(id(1).unwrap()),
        instrument: Some(instrument),
        maximum_bars: 1600,
        range: None,
        stop: Arc::new(AtomicBool::new(false)),
    }
}
fn bar() -> MarketBar {
    MarketBar {
        source_sequence: 1,
        exchange_timestamp_seconds: 1_790_800_020,
        exchange_timestamp_unix_nanos: 1_790_800_020_000_000_000,
        open: 500_000_000_000,
        high: 500_000_000_000,
        low: 500_000_000_000,
        close: 500_000_000_000,
        volume: 100_000_000,
    }
}
fn session_at(open: i64, close: i64) -> MarketSession {
    MarketSession {
        collection: MarketCollection::Cme,
        current: Some(window_at(open, open, close, close)),
        next: Some(window_at(
            close + 86_400_000_000_000,
            close + 86_400_000_000_000,
            close + 172_800_000_000_000,
            close + 172_800_000_000_000,
        )),
    }
}
fn window_at(start: i64, regular_open: i64, regular_close: i64, close: i64) -> SessionWindow {
    SessionWindow {
        start_unix_nanos: start,
        regular_open_unix_nanos: regular_open,
        regular_close_unix_nanos: regular_close,
        close_unix_nanos: close,
    }
}

#[test]
fn closed_calendar_without_a_current_window_reports_closed_until_the_next_open() {
    let session = MarketSession {
        collection: MarketCollection::Cme,
        current: None,
        next: Some(window_at(1_000, 1_000, 2_000, 2_000)),
    };
    let instrument_id = "tastytrade:Future:/ESZ6:XCME";
    let closed = session_status_at(instrument_id, session, 500);
    assert_eq!(closed.phase, MarketSessionPhase::Closed);
    assert_eq!(closed.next_open_unix_nanos, Some(1_000));
    assert_eq!(closed.session_start_unix_nanos, Some(1_000));
    assert_eq!(closed.session_end_unix_nanos, Some(2_000));
    assert_eq!(
        session_status_at(instrument_id, session, 1_500).phase,
        MarketSessionPhase::Regular
    );
    assert_eq!(
        session_status_at(instrument_id, session, 2_000).phase,
        MarketSessionPhase::Unknown
    );
}

#[test]
fn dated_equity_session_phases_follow_regular_and_extended_boundaries() {
    let mut session = session_at(100, 400);
    session.collection = MarketCollection::Equity;
    session.current = Some(window_at(100, 200, 300, 400));
    let next = session.next.unwrap();
    let instrument_id = "tastytrade:Equity:AAPL";
    for (now, expected) in [
        (99, MarketSessionPhase::Closed),
        (100, MarketSessionPhase::PreMarket),
        (199, MarketSessionPhase::PreMarket),
        (200, MarketSessionPhase::Regular),
        (299, MarketSessionPhase::Regular),
        (300, MarketSessionPhase::PostMarket),
        (399, MarketSessionPhase::PostMarket),
        (400, MarketSessionPhase::Closed),
    ] {
        assert_eq!(
            session_status_at(instrument_id, session, now).phase,
            expected,
            "{now}"
        );
    }
    let regular = session_status_at(instrument_id, session, 250);
    assert_eq!(regular.session_start_unix_nanos, Some(200));
    assert_eq!(regular.session_end_unix_nanos, Some(300));
    let extended = session_status_at(instrument_id, session, 350);
    assert_eq!(extended.session_start_unix_nanos, Some(300));
    assert_eq!(extended.session_end_unix_nanos, Some(400));
    assert_eq!(
        session_status_at(instrument_id, session, 400).next_open_unix_nanos,
        Some(next.start_unix_nanos)
    );
    assert_eq!(
        session_status_at(instrument_id, session, next.close_unix_nanos).phase,
        MarketSessionPhase::Unknown,
        "an expired calendar must not imply a holiday or continuous trading"
    );
    session.collection = MarketCollection::Cme;
    session.next = Some(window_at(1_000, 1_100, 1_200, 1_300));
    assert_eq!(
        session_status_at(instrument_id, session, 150).phase,
        MarketSessionPhase::Overnight
    );
    assert_eq!(
        session_status_at(instrument_id, session, 400).phase,
        MarketSessionPhase::Closed
    );
    assert_eq!(
        session_status_at(instrument_id, session, 999).next_open_unix_nanos,
        Some(1_000)
    );
    assert_eq!(
        session_status_at(instrument_id, session, 1_200).phase,
        MarketSessionPhase::Overnight
    );
}
#[test]
fn candle_history_waits_for_atomic_transaction_and_cannot_complete_twice() {
    let request = request();
    let symbol = candle_symbol(&request.series, request.instrument.as_ref().unwrap()).unwrap();
    let mut task = HistoryTask::collecting(request, symbol.clone(), Instant::now());
    let now = Instant::now();
    assert!(!task.expired(now));
    task.last_progress_at = now
        .checked_sub(Duration::from_secs(21))
        .expect("test clock");
    assert!(
        task.expired(now),
        "an unanswered channel must finish with a retryable failure"
    );
    task.last_progress_at = now;
    let event = |flags, bar| FeedEvent::Candle {
        channel: 7,
        symbol: symbol.clone(),
        flags,
        index: "9007199254740993".into(),
        bar,
        count: 1,
    };
    task.accept(event(SNAPSHOT_BEGIN, Some(bar())), 1, &mut 1)
        .unwrap();
    task.accept(event(SNAPSHOT_END | TX_PENDING, None), 1, &mut 1)
        .unwrap();
    assert_eq!(task.candle_state, CandleHistoryState::Ending);
    task.accept(event(0, Some(bar())), 1, &mut 1).unwrap();
    assert_eq!(task.candle_state, CandleHistoryState::Ready);
    let open = bar().exchange_timestamp_unix_nanos;
    let forming = task
        .snapshot_at(
            open + 30_000_000_000,
            Some(session_at(open, open + 60_000_000_000)),
        )
        .unwrap()
        .forming
        .unwrap();
    assert_eq!(forming.bar, bar());
    assert_eq!(forming.trades, Some(1));
    assert!(
        task.snapshot_at(
            open + 60_000_000_000,
            Some(session_at(open, open + 60_000_000_000))
        )
        .unwrap()
        .forming
        .is_none()
    );
    assert!(
        task.snapshot_at(
            open + 30_000_000_000,
            Some(session_at(open - 60_000_000_000, open + 10_000_000_000))
        )
        .unwrap()
        .forming
        .is_none()
    );
    assert!(task.snapshot_at(open + 30_000_000_000, None).is_err());
    task.candle_state = CandleHistoryState::Published;
    task.accept(event(SNAPSHOT_BEGIN, None), 1, &mut 1).unwrap();
    assert_eq!(task.candle_state, CandleHistoryState::Published);
    assert_eq!(task.candles.len(), 1);
}
#[test]
fn newer_only_dxlink_snapshot_marks_backwards_history_exhausted() {
    let mut request = request();
    let newest = bar();
    request.range = Some(super::super::HistoryRange {
        start_unix_nanos: newest.exchange_timestamp_unix_nanos - 3_600_000_000_000,
        end_unix_nanos: newest.exchange_timestamp_unix_nanos - 60_000_000_000,
    });
    let symbol = candle_symbol(&request.series, request.instrument.as_ref().unwrap()).unwrap();
    let mut task = HistoryTask::collecting(request, symbol.clone(), Instant::now());
    task.accept(
        FeedEvent::Candle {
            channel: 7,
            symbol,
            flags: SNAPSHOT_END,
            index: "1".into(),
            bar: Some(newest),
            count: 1,
        },
        1,
        &mut 1,
    )
    .unwrap();
    assert_eq!(task.candle_state, CandleHistoryState::Ready);
    let snapshot = task
        .snapshot_at(newest.exchange_timestamp_unix_nanos, None)
        .unwrap();
    assert_eq!(snapshot.bars, [] as [MarketBar; 0]);
    assert!(snapshot.backwards_exhausted);
}
/// Bars delivered newest first, as dxFeed publishes a time-series snapshot,
/// with the first one carrying `SNAPSHOT_BEGIN`.
fn newest_first_snapshot(task: &mut HistoryTask, period_nanos: i64, bars: i64) -> Vec<MarketBar> {
    let newest = bar();
    let mut delivered = Vec::new();
    for age in 0..bars {
        let time = newest.exchange_timestamp_unix_nanos - age * period_nanos;
        let bar = MarketBar {
            exchange_timestamp_seconds: time / 1_000_000_000,
            exchange_timestamp_unix_nanos: time,
            ..newest
        };
        task.accept(
            FeedEvent::Candle {
                channel: 7,
                symbol: task.symbol.clone(),
                flags: if age == 0 { SNAPSHOT_BEGIN } else { 0 },
                index: (bars - age).to_string(),
                bar: Some(bar),
                count: 1,
            },
            1,
            &mut 1,
        )
        .unwrap();
        delivered.push(bar);
    }
    delivered.reverse();
    for (sequence, bar) in (1..).zip(&mut delivered) {
        bar.source_sequence = sequence;
    }
    delivered
}
#[test]
fn a_snapshot_that_stops_without_its_end_marker_keeps_the_delivered_newest_range() {
    let mut request = request();
    request.series.period = BarPeriod::session(1).unwrap();
    let symbol = candle_symbol(&request.series, request.instrument.as_ref().unwrap()).unwrap();
    let started_at = Instant::now();
    let mut task = HistoryTask::collecting(request, symbol, started_at);
    let delivered = newest_first_snapshot(&mut task, 86_400_000_000_000, 276);
    let streaming = task.last_progress_at
        + CANDLE_SNAPSHOT_STALL
            .checked_sub(Duration::from_millis(1))
            .expect("stall window");
    let quiet = task.last_progress_at + CANDLE_SNAPSHOT_STALL;
    assert!(
        !task.close_stalled_snapshot(streaming),
        "a snapshot still streaming is not cut short"
    );
    assert_eq!(task.candle_state, CandleHistoryState::Collecting);
    assert!(task.close_stalled_snapshot(quiet));
    assert_eq!(task.candle_state, CandleHistoryState::Ready);
    let snapshot = task
        .snapshot_at(
            delivered[delivered.len() - 1].exchange_timestamp_unix_nanos,
            None,
        )
        .unwrap();
    assert_eq!(snapshot.bars, delivered);
    assert!(
        !snapshot.backwards_exhausted,
        "older history may still exist behind a cut-off snapshot"
    );
}
#[test]
fn a_channel_that_never_begins_its_snapshot_still_fails() {
    let request = request();
    let symbol = candle_symbol(&request.series, request.instrument.as_ref().unwrap()).unwrap();
    let started_at = Instant::now();
    let mut task = HistoryTask::collecting(request, symbol, started_at);
    let silent = started_at + Duration::from_secs(21);
    assert!(!task.close_stalled_snapshot(silent));
    assert_eq!(task.candle_state, CandleHistoryState::Collecting);
    assert!(task.expired(silent));
}
/// Mirrors the observed `/NQZ6` 5m sequence: the covering snapshot stops after
/// 578 bars without an end marker, then the backfill for the range before its
/// oldest bar receives the same newest-first bars and stops again.
#[test]
fn a_backfill_cut_off_above_its_range_exhausts_older_history() {
    const FIVE_MINUTES: i64 = 300_000_000_000;
    let mut covering = request();
    covering.series.period = BarPeriod::time(300).unwrap();
    let symbol = candle_symbol(&covering.series, covering.instrument.as_ref().unwrap()).unwrap();
    let mut task = HistoryTask::collecting(covering.clone(), symbol.clone(), Instant::now());
    let retained = newest_first_snapshot(&mut task, FIVE_MINUTES, 578);
    assert!(task.close_stalled_snapshot(task.last_progress_at + CANDLE_SNAPSHOT_STALL));
    let closed = retained[retained.len() - 1].exchange_timestamp_unix_nanos + FIVE_MINUTES;
    assert_eq!(task.snapshot_at(closed, None).unwrap().bars, retained);

    let oldest = retained[0].exchange_timestamp_unix_nanos;
    let mut backfill = covering;
    backfill.range = Some(super::super::HistoryRange {
        start_unix_nanos: oldest - 7_680 * FIVE_MINUTES,
        end_unix_nanos: oldest,
    });
    for end in [None, Some(SNAPSHOT_SNIP)] {
        let mut task = HistoryTask::collecting(backfill.clone(), symbol.clone(), Instant::now());
        newest_first_snapshot(&mut task, FIVE_MINUTES, 578);
        match end {
            None => {
                assert!(task.close_stalled_snapshot(task.last_progress_at + CANDLE_SNAPSHOT_STALL));
            }
            Some(flags) => task
                .accept(
                    FeedEvent::Candle {
                        channel: 7,
                        symbol: symbol.clone(),
                        flags,
                        index: "0".into(),
                        bar: None,
                        count: 0,
                    },
                    1,
                    &mut 1,
                )
                .unwrap(),
        }
        assert_eq!(task.candle_state, CandleHistoryState::Ready);
        let snapshot = task
            .snapshot_at(closed, None)
            .expect("a newest-first snapshot cut off above the range answers it");
        assert_eq!(snapshot.bars, [] as [MarketBar; 0]);
        assert!(
            snapshot.backwards_exhausted,
            "retrying the subscription receives the same cut-off snapshot"
        );
    }
}
#[test]
fn a_cut_off_snapshot_that_passes_its_range_reports_the_range_empty() {
    let mut request = request();
    let newest = bar();
    request.range = Some(super::super::HistoryRange {
        start_unix_nanos: newest.exchange_timestamp_unix_nanos - 7_200_000_000_000,
        end_unix_nanos: newest.exchange_timestamp_unix_nanos - 3_600_000_000_000,
    });
    let symbol = candle_symbol(&request.series, request.instrument.as_ref().unwrap()).unwrap();
    let mut task = HistoryTask::collecting(request, symbol.clone(), Instant::now());
    let older = newest.exchange_timestamp_unix_nanos - 10_800_000_000_000;
    for (flags, index, bar) in [
        (SNAPSHOT_BEGIN, "2", newest),
        (
            0,
            "1",
            MarketBar {
                exchange_timestamp_seconds: older / 1_000_000_000,
                exchange_timestamp_unix_nanos: older,
                ..newest
            },
        ),
    ] {
        task.accept(
            FeedEvent::Candle {
                channel: 7,
                symbol: symbol.clone(),
                flags,
                index: index.into(),
                bar: Some(bar),
                count: 1,
            },
            1,
            &mut 1,
        )
        .unwrap();
    }
    assert!(task.close_stalled_snapshot(task.last_progress_at + CANDLE_SNAPSHOT_STALL));
    let snapshot = task
        .snapshot_at(newest.exchange_timestamp_unix_nanos, None)
        .expect("delivery passed through the whole range");
    assert_eq!(snapshot.bars, [] as [MarketBar; 0]);
    assert!(
        !snapshot.backwards_exhausted,
        "a bar older than the range proves older history exists"
    );
}
#[test]
fn a_cut_off_snapshot_without_any_candle_proves_nothing() {
    let newest = bar();
    for range in [
        None,
        Some(super::super::HistoryRange {
            start_unix_nanos: newest.exchange_timestamp_unix_nanos - 7_200_000_000_000,
            end_unix_nanos: newest.exchange_timestamp_unix_nanos - 3_600_000_000_000,
        }),
    ] {
        let mut request = request();
        request.range = range;
        let symbol = candle_symbol(&request.series, request.instrument.as_ref().unwrap()).unwrap();
        let mut task = HistoryTask::collecting(request, symbol.clone(), Instant::now());
        task.accept(
            FeedEvent::Candle {
                channel: 7,
                symbol,
                flags: SNAPSHOT_BEGIN | REMOVE_EVENT,
                index: "2".into(),
                bar: None,
                count: 0,
            },
            1,
            &mut 1,
        )
        .unwrap();
        assert!(task.close_stalled_snapshot(task.last_progress_at + CANDLE_SNAPSHOT_STALL));
        assert!(
            task.snapshot_at(newest.exchange_timestamp_unix_nanos, None)
                .is_err(),
            "a truncated snapshot without a candle proves neither empty nor exhausted history"
        );
    }
}
#[test]
fn tape_backfill_keeps_only_its_requested_window_and_applies_corrections() {
    let instrument = instrument();
    let from = bar().exchange_timestamp_unix_nanos;
    let mut task = TapeTask {
        instrument: instrument.clone(),
        channel: 7,
        deadline: Instant::now() + Duration::from_secs(15),
        from_nanos: from,
        until_nanos: from + 60_000_000_000,
        trades: BTreeMap::new(),
        end_seen: false,
        complete: false,
        truncated: false,
    };
    let print = |time_nanos, price| TradePrint {
        time_nanos,
        price,
        quantity: 100_000_000,
        bid_price: None,
        ask_price: None,
        aggressor: aeris_market_data::AggressorSide::Unknown,
        spread_leg: false,
    };
    let event = |flags, index: &str, kind: &str, trade| FeedEvent::Trade {
        channel: 7,
        symbol: instrument.provider_symbol.clone(),
        flags,
        index: index.into(),
        kind: kind.into(),
        trade,
    };
    let mut ordinal = 0;
    task.accept(
        event(
            SNAPSHOT_BEGIN | TX_PENDING,
            "1",
            "NEW",
            Some(print(from + 1, 500_000_000_000)),
        ),
        1,
        &mut ordinal,
    )
    .unwrap();
    task.accept(
        event(
            0,
            "2",
            "NEW",
            Some(print(from + 60_000_000_000, 500_000_000_000)),
        ),
        1,
        &mut ordinal,
    )
    .unwrap();
    assert_eq!(task.trades.len(), 1);
    task.accept(
        event(0, "1", "CORRECTION", Some(print(from + 1, 500_000_000_001))),
        1,
        &mut ordinal,
    )
    .unwrap();
    assert_eq!(task.trades["1"].price, 500_000_000_001);
    task.accept(event(SNAPSHOT_SNIP, "0", "NEW", None), 1, &mut ordinal)
        .unwrap();
    assert!(task.complete);
    assert!(task.truncated);
    task.accept(event(REMOVE_EVENT, "1", "CANCEL", None), 1, &mut ordinal)
        .unwrap();
    assert!(task.trades.is_empty());
}
#[test]
fn newer_catalog_search_cancels_the_prior_request_for_its_consumer() {
    let api = BrokerApi::default();
    api.register_search(1, 1, false).unwrap();
    let prior = Arc::clone(&api.search_control.lock().unwrap()[&1].1);
    api.register_search(1, 2, false).unwrap();
    assert!(prior.load(Ordering::Acquire));
    let current = Arc::clone(&api.search_control.lock().unwrap()[&1].1);
    assert!(!current.load(Ordering::Acquire));
    api.register_search(1, 1, false).unwrap();
    assert!(!current.load(Ordering::Acquire));
    api.cancel_searches();
    assert!(current.load(Ordering::Acquire));
}

#[test]
fn startup_search_does_not_retire_the_user_search() {
    let api = BrokerApi::default();
    api.register_search(66, 17, true).unwrap();
    api.register_search(66, 1, false).unwrap();
    assert_eq!(api.startup_search_control.lock().unwrap()[&66].0, 17);
    assert_eq!(api.search_control.lock().unwrap()[&66].0, 1);
}

#[test]
fn startup_search_and_user_search_are_independent() {
    let api = BrokerApi::default();
    api.register_search(66, 1, false).unwrap();
    api.register_search(66, 17, true).unwrap();
    assert_eq!(api.startup_search_control.lock().unwrap()[&66].0, 17);
    assert_eq!(api.search_control.lock().unwrap()[&66].0, 1);
}
#[test]
fn futures_search_uses_the_primed_catalog_before_remote_equity_search() {
    let api = BrokerApi::default();
    let future: FutureInstrument = serde_json::from_str(
            r#"{"symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME","product-code":"ES","expiration-date":"2026-12-18","active":true,"active-month":true,"notional-multiplier":"50.0","tick-size":"0.25"}"#,
        )
        .unwrap();
    let mut futures = vec![future];
    let mut searches = CatalogSearches::new();
    let search = SearchProviderInstruments {
        consumer_id: 7,
        search_generation: 2,
        startup_resolve: false,
        provider: "tastytrade".into(),
        query: "ES".into(),
        maximum_results: 10,
        categories: aeris_contracts::InstrumentSearchCategories::ALL,
    };
    let event = search_catalog(
        &search,
        &mut futures,
        &mut searches,
        &api,
        &Arc::new(AtomicBool::new(false)),
        &Arc::new(AtomicU64::new(1)),
    )
    .unwrap();
    let CatalogEvent::Search(result) = event else {
        panic!("expected local futures search result");
    };
    assert_eq!(result.instruments[0].symbol, "/ESZ6");
    assert_eq!(searches[&(7, false)].0, 2);
    assert_eq!(searches[&(7, false)].1[0].symbol, "/ESZ6");
    assert_eq!(
        ResolvedInstrument::from_future(&futures[0])
            .unwrap()
            .streamer_symbol,
        "/ESZ26:XCME"
    );
    let calendar_revision = api.calendar_revision();
    api.clear().unwrap();
    assert_eq!(api.authorization_epoch.load(Ordering::Acquire), 1);
    assert_ne!(
        api.calendar_revision(),
        calendar_revision,
        "statuses computed from a cleared calendar must be recomputed"
    );
}
#[test]
fn a_picker_search_cannot_retire_the_pending_startup_selection() {
    let api = BrokerApi::default();
    let future: FutureInstrument = serde_json::from_str(
            r#"{"symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME","product-code":"ES","expiration-date":"2026-12-18","active":true,"active-month":true,"notional-multiplier":"50.0","tick-size":"0.25"}"#,
        )
        .unwrap();
    let mut futures = vec![future];
    let mut searches = CatalogSearches::new();
    let stop = Arc::new(AtomicBool::new(false));
    let generation = Arc::new(AtomicU64::new(1));
    for (search_generation, query, startup_resolve) in [(17, "ESZ6", true), (4, "", false)] {
        let search = SearchProviderInstruments {
            consumer_id: 7,
            search_generation,
            startup_resolve,
            provider: "tastytrade".into(),
            query: query.into(),
            maximum_results: 10,
            categories: aeris_contracts::InstrumentSearchCategories::ALL,
        };
        search_catalog(
            &search,
            &mut futures,
            &mut searches,
            &api,
            &stop,
            &generation,
        )
        .unwrap();
    }
    let selection = SelectProviderInstrument {
        consumer_id: 7,
        selection_generation: 17,
        search_generation: 17,
        startup_resolve: true,
        provider: "tastytrade".into(),
        symbol: "/ESZ6".into(),
        exchange: "CME".into(),
        entitlement_id: ENTITLEMENT.into(),
    };
    let selected = resolve_selection(
        &selection,
        &futures,
        &searches,
        &api,
        &stop,
        &generation,
        &mut 0,
    )
    .unwrap();
    assert!(matches!(
        selected,
        CatalogEvent::Selection { command_generation, instrument, .. }
            if command_generation == 17
                && instrument.provider_symbol == "/ESZ26:XCME"
    ));
}
#[test]
fn equity_search_worker_debounces_to_the_latest_consumer_query() {
    let api = BrokerApi::default();
    api.register_search(7, 1, false).unwrap();
    api.register_search(7, 2, false).unwrap();
    api.state.lock().unwrap().searches.insert(
        "SPY".into(),
        (
            Instant::now(),
            vec![SearchInstrument {
                symbol: "SPY".into(),
                instrument_type: "Equity".into(),
                exchange: Some("NYSE".into()),
                description: None,
            }],
        ),
    );
    let (requests, incoming) = mpsc::sync_channel(2);
    let (outgoing, replies) = mpsc::sync_channel(2);
    for (generation, query) in [(1, "S"), (2, "SPY")] {
        requests
            .send(RemoteSearchRequest {
                search: SearchProviderInstruments {
                    consumer_id: 7,
                    search_generation: generation,
                    startup_resolve: false,
                    provider: "tastytrade".into(),
                    query: query.into(),
                    maximum_results: 10,
                    categories: aeris_contracts::InstrumentSearchCategories::ALL,
                },
                authorization_epoch: 0,
            })
            .unwrap();
    }
    drop(requests);
    run_remote_search(
        &incoming,
        &outgoing,
        &api,
        &Arc::new(AtomicBool::new(false)),
    );
    let reply = replies.try_recv().unwrap();
    assert_eq!(reply.request.search.search_generation, 2);
    assert_eq!(reply.result.unwrap()[0].symbol, "SPY");
    assert!(replies.try_recv().is_err());
}
#[test]
fn futures_selection_does_not_wait_for_remote_equity_search() {
    let api = Arc::new(BrokerApi::default());
    let future: FutureInstrument = serde_json::from_str(
            r#"{"symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME","product-code":"ES","expiration-date":"2026-12-18","active":true,"active-month":true,"notional-multiplier":"50.0","tick-size":"0.25"}"#,
        )
        .unwrap();
    api.state.lock().unwrap().futures = Some((Instant::now(), vec![future]));
    api.register_search(7, 1, false).unwrap();
    let (control_tx, control_rx) = mpsc::sync_channel(4);
    let (event_tx, event_rx) = mpsc::sync_channel(4);
    let (wake_tx, _wake_rx) = mpsc::sync_channel(4);
    let publisher = CatalogPublisher::new(
        event_tx,
        "tastytrade",
        ProviderCoordinatorWake::new(wake_tx, ["tastytrade"]),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_api = Arc::clone(&api);
    let worker = thread::spawn(move || {
        run_catalog(
            &control_rx,
            &publisher,
            &Arc::new(AtomicU64::new(1)),
            &worker_stop,
            &worker_api,
        );
    });
    control_tx
        .send(CatalogControl::Search(SearchProviderInstruments {
            consumer_id: 7,
            search_generation: 1,
            startup_resolve: false,
            provider: "tastytrade".into(),
            query: "ES".into(),
            maximum_results: 10,
            categories: aeris_contracts::InstrumentSearchCategories::ALL,
        }))
        .unwrap();
    control_tx
        .send(CatalogControl::Select(SelectProviderInstrument {
            consumer_id: 7,
            selection_generation: 1,
            search_generation: 1,
            startup_resolve: false,
            provider: "tastytrade".into(),
            symbol: "/ESZ6".into(),
            exchange: "CME".into(),
            entitlement_id: ENTITLEMENT.into(),
        }))
        .unwrap();
    assert!(matches!(
        event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        CatalogEvent::SearchPreliminary(_)
    ));
    let selected = event_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(selected, CatalogEvent::Selection { instrument, .. } if instrument.provider_symbol == "/ESZ26:XCME")
    );
    stop.store(true, Ordering::Release);
    drop(control_tx);
    worker.join().unwrap();
}
#[test]
fn live_snapshot_end_marker_publishes_the_batch_and_retired_symbols_are_ignored() {
    let (_, controls) = mpsc::sync_channel(1);
    let (events, output) = mpsc::sync_channel(4);
    let (_, history) = mpsc::sync_channel(1);
    let (completions, _) = mpsc::sync_channel(1);
    let (token_requests, _) = mpsc::sync_channel(1);
    let (_, token_replies) = mpsc::sync_channel(1);
    let wake = ProviderCoordinatorWake::new(completions.clone(), ["tastytrade"]);
    let mut worker = Worker::new(WorkerPorts {
        api: Arc::new(BrokerApi::default()),
        controls,
        events,
        history,
        completions,
        generation: Arc::new(AtomicU64::new(1)),
        stop: Arc::new(AtomicBool::new(false)),
        wake,
        token_requests,
        token_replies,
    });
    let request = request();
    let instrument = request.instrument.unwrap();
    let symbol = candle_symbol(&request.series, &instrument).unwrap();
    worker.demand.series.push((request.series, instrument));
    worker
        .accept_candle(&symbol, SNAPSHOT_BEGIN, Some(bar()), 1)
        .unwrap();
    assert!(output.try_recv().is_err());
    worker
        .accept_candle(&symbol, SNAPSHOT_END | REMOVE_EVENT, None, 0)
        .unwrap();
    assert!(matches!(
        output.try_recv().unwrap(),
        RealtimeEvent::Candle(1, ..)
    ));
    worker
        .accept_candle("retired{=m}", 0, Some(bar()), 1)
        .unwrap();
    assert!(output.try_recv().is_err());
    assert!(worker.socket.is_none());
}

#[test]
fn trade_updates_quote_prices_without_refreshing_quote_sizes() {
    let (_, controls) = mpsc::sync_channel(1);
    let (events, output) = mpsc::sync_channel(8);
    let (_, history) = mpsc::sync_channel(1);
    let (completions, _) = mpsc::sync_channel(1);
    let (token_requests, _) = mpsc::sync_channel(1);
    let (_, token_replies) = mpsc::sync_channel(1);
    let wake = ProviderCoordinatorWake::new(completions.clone(), ["tastytrade"]);
    let mut worker = Worker::new(WorkerPorts {
        api: Arc::new(BrokerApi::default()),
        controls,
        events,
        history,
        completions,
        generation: Arc::new(AtomicU64::new(1)),
        stop: Arc::new(AtomicBool::new(false)),
        wake,
        token_requests,
        token_replies,
    });
    let instrument = instrument();
    let symbol = instrument.provider_symbol.clone();
    worker.demand.instruments.push(instrument.clone());
    let mut quote_metadata = metadata(&instrument, 1, 1, None).unwrap();
    quote_metadata.timestamps.provider_unix_nanos = Some(1_790_800_000_000_000_000);
    worker
        .accept(FeedEvent::Quote {
            channel: 1,
            symbol: symbol.clone(),
            bid: Some((500_000_000_000, 2_000_000_000)),
            ask: Some((500_100_000_000, 3_000_000_000)),
            time_nanos: quote_metadata.timestamps.provider_unix_nanos,
        })
        .unwrap();
    let initial = output.try_recv().unwrap();
    assert!(matches!(initial, RealtimeEvent::Quote(1, _)));

    worker
        .accept(FeedEvent::Trade {
            channel: 5,
            symbol,
            flags: 0,
            index: "trade-1".into(),
            kind: "NEW".into(),
            trade: Some(TradePrint {
                time_nanos: 1_790_800_001_000_000_000,
                price: 500_050_000_000,
                quantity: 1_000_000_000,
                bid_price: Some(500_025_000_000),
                ask_price: Some(500_125_000_000),
                aggressor: aeris_market_data::AggressorSide::Buy,
                spread_leg: false,
            }),
        })
        .unwrap();
    let updated = output.try_recv().unwrap();
    let RealtimeEvent::Quote(_, updated) = updated else {
        panic!("trade should publish a quote price update");
    };
    let bid = updated.bid.unwrap();
    let ask = updated.ask.unwrap();
    assert_eq!(bid.price, 500_025_000_000);
    assert_eq!(ask.price, 500_125_000_000);
    assert_eq!(bid.quantity, Some(2_000_000_000));
    assert_eq!(ask.quantity, Some(3_000_000_000));
    assert_eq!(
        updated.metadata.timestamps.provider_unix_nanos,
        quote_metadata.timestamps.provider_unix_nanos
    );
}

fn live_worker(event_capacity: usize) -> (Worker, Receiver<RealtimeEvent>) {
    let (_, controls) = mpsc::sync_channel(1);
    let (events, output) = mpsc::sync_channel(event_capacity);
    let (_, history) = mpsc::sync_channel(1);
    let (completions, _) = mpsc::sync_channel(1);
    let (token_requests, _) = mpsc::sync_channel(1);
    let (_, token_replies) = mpsc::sync_channel(1);
    let wake = ProviderCoordinatorWake::new(completions.clone(), ["tastytrade"]);
    let worker = Worker::new(WorkerPorts {
        api: Arc::new(BrokerApi::default()),
        controls,
        events,
        history,
        completions,
        generation: Arc::new(AtomicU64::new(1)),
        stop: Arc::new(AtomicBool::new(false)),
        wake,
        token_requests,
        token_replies,
    });
    (worker, output)
}

#[test]
fn locked_or_crossed_quotes_are_withheld_without_failing_the_session() {
    let (mut worker, output) = live_worker(4);
    let instrument = instrument();
    let symbol = instrument.provider_symbol.clone();
    worker.demand.instruments.push(instrument);
    let quote = |bid: i64, ask: i64| FeedEvent::Quote {
        channel: 1,
        symbol: symbol.clone(),
        bid: Some((bid, 1_000_000_000)),
        ask: Some((ask, 1_000_000_000)),
        time_nanos: Some(1_790_800_000_000_000_000),
    };

    worker
        .accept(quote(500_100_000_000, 500_000_000_000))
        .expect("a crossed quote is not a transport failure");
    worker
        .accept(quote(500_000_000_000, 500_000_000_000))
        .expect("a locked quote is not a transport failure");
    assert!(output.try_recv().is_err());
    assert!(worker.quotes.is_empty());

    worker
        .accept(quote(500_000_000_000, 500_100_000_000))
        .expect("a valid quote publishes after withheld quotes");
    let RealtimeEvent::Quote(1, published) = output.try_recv().unwrap() else {
        panic!("a valid quote should publish");
    };
    assert_eq!(published.bid.unwrap().price, 500_000_000_000);
    assert_eq!(published.ask.unwrap().price, 500_100_000_000);
}

#[test]
fn repeated_transport_failures_keep_recovering_with_capped_backoff() {
    let (mut worker, output) = live_worker(16);
    for attempt in 1..=8 {
        let before = Instant::now();
        worker.recover(format!("DXLink heartbeat expired ({attempt})"));
        assert!(
            !worker.paused,
            "transport failure {attempt} parked the session"
        );
        assert!(matches!(
            output.try_recv().unwrap(),
            RealtimeEvent::Recovering(1, _)
        ));
        assert!(worker.retry_at <= before + Duration::from_secs(48) + Duration::from_secs(1));
    }
    assert_eq!(worker.backoff.attempts(), 8);
}

#[test]
fn server_rate_limit_overrides_tastytrade_reconnect_deadline() {
    let (mut worker, output) = live_worker(4);
    let before = Instant::now();
    worker.recover_with_class(
        "Tastytrade API rate limited".into(),
        crate::market_service::reconnect::RetryClass::RateLimited {
            retry_after: Duration::from_secs(17),
        },
    );
    assert!(worker.retry_at >= before + Duration::from_secs(17));
    assert!(worker.retry_at <= Instant::now() + Duration::from_secs(17));
    assert_eq!(worker.backoff.attempts(), 0);
    assert!(matches!(
        output.try_recv().unwrap(),
        RealtimeEvent::Recovering(1, detail) if detail == "Tastytrade API rate limited"
    ));
}
