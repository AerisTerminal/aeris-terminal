#![cfg(test)]

use super::*;
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
fn equity_query_with_loaded_futures_catalog_returns_preliminary_empty_result() {
    let search = SearchProviderInstruments {
        consumer_id: 1,
        search_generation: 1,
        provider: "tastytrade".into(),
        query: "SPY".into(),
        maximum_results: 100,
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
    assert!(result.instruments.is_empty());
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
                provider: "tastytrade".into(),
                query: query.into(),
                maximum_results: 10,
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
fn no_match_query_returns_empty_search_result() {
    let search = SearchProviderInstruments {
        consumer_id: 1,
        search_generation: 1,
        provider: "tastytrade".into(),
        query: "/NO_SUCH_FUTURE".into(),
        maximum_results: 100,
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
    assert!(result.instruments.is_empty());
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
    assert_eq!(
        install_resolved(resolved, 1, 1).price_increment,
        Some(1_000_000)
    );
}

#[test]
fn late_candle_subscription_starts_at_demand_time() {
    let mut starts = BTreeMap::new();
    assert_eq!(candle_start_ms(&mut starts, "ES{=1m}".into(), 100), 100);
    assert_eq!(candle_start_ms(&mut starts, "ES{=1m}".into(), 200), 100);
    assert_eq!(candle_start_ms(&mut starts, "NQ{=1m}".into(), 200), 200);
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
    HistoryRequest {
        series: BarSeriesKey {
            provider_id: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: ENTITLEMENT.into(),
            period: BarPeriod::time(60).unwrap(),
            definition_version: 1,
        },
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
        start_unix_nanos: open,
        close_unix_nanos: close,
        next_start_unix_nanos: close + 86_400_000_000_000,
        next_close_unix_nanos: close + 172_800_000_000_000,
    }
}
#[test]
fn candle_history_waits_for_atomic_transaction_and_cannot_complete_twice() {
    let request = request();
    let symbol = candle_symbol(&request.series, request.instrument.as_ref().unwrap()).unwrap();
    let mut task = HistoryTask {
        request,
        symbol: symbol.clone(),
        deadline: Instant::now() + Duration::from_secs(45),
        candles: BTreeMap::new(),
        saw_newer_candle: false,
        candle_state: CandleHistoryState::Collecting,
    };
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
    assert!(task.candle_state == CandleHistoryState::Ending);
    task.accept(event(0, Some(bar())), 1, &mut 1).unwrap();
    assert!(task.candle_state == CandleHistoryState::Ready);
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
    assert!(task.candle_state == CandleHistoryState::Published);
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
    let mut task = HistoryTask {
        request,
        symbol: symbol.clone(),
        deadline: Instant::now() + Duration::from_secs(45),
        candles: BTreeMap::new(),
        saw_newer_candle: false,
        candle_state: CandleHistoryState::Collecting,
    };
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
    assert!(task.candle_state == CandleHistoryState::Ready);
    let snapshot = task
        .snapshot_at(newest.exchange_timestamp_unix_nanos, None)
        .unwrap();
    assert!(snapshot.bars.is_empty());
    assert!(snapshot.backwards_exhausted);
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
    api.register_search(1, 1).unwrap();
    let prior = Arc::clone(&api.search_control.lock().unwrap()[&1].1);
    api.register_search(1, 2).unwrap();
    assert!(prior.load(Ordering::Acquire));
    let current = Arc::clone(&api.search_control.lock().unwrap()[&1].1);
    assert!(!current.load(Ordering::Acquire));
    api.register_search(1, 1).unwrap();
    assert!(!current.load(Ordering::Acquire));
    api.cancel_searches();
    assert!(current.load(Ordering::Acquire));
}

#[test]
fn startup_catalog_generation_is_retired_by_the_first_user_search() {
    let api = BrokerApi::default();
    api.register_search(66, u64::from(u32::MAX)).unwrap();
    api.register_search(66, 1).unwrap();
    assert_eq!(api.search_control.lock().unwrap()[&66].0, 1);
}

#[test]
fn startup_catalog_generation_cannot_replace_a_user_search() {
    let api = BrokerApi::default();
    api.register_search(66, 1).unwrap();
    api.register_search(66, u64::from(u32::MAX)).unwrap();
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
    let mut searches = BTreeMap::new();
    let search = SearchProviderInstruments {
        consumer_id: 7,
        search_generation: 2,
        provider: "tastytrade".into(),
        query: "ES".into(),
        maximum_results: 10,
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
    assert_eq!(searches[&7].0, 2);
    assert_eq!(searches[&7].1[0].symbol, "/ESZ6");
    assert_eq!(
        ResolvedInstrument::from_future(&futures[0])
            .unwrap()
            .streamer_symbol,
        "/ESZ26:XCME"
    );
    api.clear().unwrap();
    assert_eq!(api.authorization_epoch.load(Ordering::Acquire), 1);
}
#[test]
fn equity_search_worker_debounces_to_the_latest_consumer_query() {
    let api = BrokerApi::default();
    api.register_search(7, 1).unwrap();
    api.register_search(7, 2).unwrap();
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
                    provider: "tastytrade".into(),
                    query: query.into(),
                    maximum_results: 10,
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
    api.register_search(7, 1).unwrap();
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
            provider: "tastytrade".into(),
            query: "ES".into(),
            maximum_results: 10,
        }))
        .unwrap();
    control_tx
        .send(CatalogControl::Select(SelectProviderInstrument {
            consumer_id: 7,
            selection_generation: 1,
            search_generation: 1,
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
    assert_eq!(bid.quantity, 2_000_000_000);
    assert_eq!(ask.quantity, 3_000_000_000);
    assert_eq!(
        updated.metadata.timestamps.provider_unix_nanos,
        quote_metadata.timestamps.provider_unix_nanos
    );
}
