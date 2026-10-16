use super::*;
use aeris_instruments::{CalendarSourceKind, SessionPhase};
use aeris_trading::market::{
    FeedReason, FeedState, InstrumentTerms, MarketFeedError, MarketFeedEvent, MarketRoute,
    QuoteInterest, QuoteInterestSink, TradingQuote,
};

fn route() -> MarketRoute {
    let instrument = instrument();
    MarketRoute {
        provider_id: "fixture".into(),
        instrument_id: instrument.instrument_id,
        provider_symbol: "ESZ6".into(),
        display_symbol: "ESZ6".into(),
        venue_id: "CME".into(),
        entitlement_id: Some("fixture-entitlement".into()),
        price_scale: 2,
        quantity_scale: 0,
    }
}

fn terms() -> InstrumentTerms {
    let instrument = instrument();
    InstrumentTerms {
        route: route(),
        instrument_class: instrument.contract.instrument_class,
        price_grid: instrument.contract.price_grid,
        point_value: instrument.contract.point_value,
        quantity_increment: instrument.contract.order_quantity_increment,
        currency: instrument.contract.currency,
        expiry: instrument.contract.expiry,
        first_notice: instrument.contract.first_notice,
        last_trade: instrument.contract.last_trade,
        session_hours: instrument.contract.session_hours,
        session_generation: 1,
    }
}

fn quote(generation: u64, sequence: u64, bid: i64, ask: i64) -> TradingQuote {
    TradingQuote {
        instrument_id: route().instrument_id,
        bid: FixedPoint::try_new(bid, 2).unwrap(),
        ask: FixedPoint::try_new(ask, 2).unwrap(),
        bid_size: None,
        ask_size: None,
        last: None,
        provider_id: "fixture".into(),
        provider_generation: generation,
        source_sequence: Some(sequence),
        exchange_unix_nanos: None,
        received_unix_nanos: 1_000_000_000_000 + i64::try_from(sequence).unwrap() * 1_000_000,
    }
}

fn feed_service(directory: &TestDirectory) -> (TradingService, Arc<crate::clock::ManualClock>) {
    let clock = Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(directory);
    configuration.clock = clock.clone();
    (start_service_with_config(configuration), clock)
}

fn live_session(id: InstrumentId, generation: u64) -> MarketFeedEvent {
    MarketFeedEvent::Session(aeris_trading::market::InstrumentSession {
        instrument_id: id,
        provider_generation: generation,
        phase: SessionPhase::Open,
        source: CalendarSourceKind::ProviderCalendar,
        session_end_unix_nanos: Some(2_000_000_000_000),
        next_open_unix_nanos: None,
    })
}

fn feed_state(id: InstrumentId, generation: u64, state: FeedState) -> MarketFeedEvent {
    MarketFeedEvent::Feed {
        instrument_id: id,
        state,
        provider_generation: generation,
    }
}

#[test]
fn chart_bracket_uses_verified_grid_without_fixed_tick_and_rests() {
    let directory = TestDirectory::new("chart-significant-grid");
    let (service, _clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .unwrap();
    let mut verified = terms();
    verified.price_grid = aeris_instruments::PriceGrid::SignificantFigures {
        figures: 5,
        max_decimals: 2,
    };
    feed.push(MarketFeedEvent::Terms(verified)).unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    service.status().unwrap();
    let mut entry = market_order("chart-significant", OrderSide::Buy, 2, 1_000);
    entry.order_type = OrderType::Limit;
    entry.limit_price = Some(FixedPoint::try_new(9_999, 2).unwrap());
    let placed = service
        .place_chart_bracket_from_market(PlaceChartBracket {
            entry,
            stop_price: FixedPoint::try_new(9_996, 2).unwrap(),
            target_price: FixedPoint::try_new(10_003, 2).unwrap(),
        })
        .expect("chart bracket accepts significant-figure grid");
    let BracketPlacement::Managed(bracket) = placed else {
        panic!("expected managed bracket");
    };
    assert_eq!(bracket.template.stop_offset_ticks, 3);
    assert_eq!(bracket.template.targets[0].offset_ticks, 4);
    assert_eq!(service.snapshot().unwrap().orders.len(), 1);
    service.shutdown(Duration::from_secs(2)).unwrap();
}

#[test]
fn all_practice_entry_shapes_obey_provider_neutral_session_phases() {
    for phase in [
        SessionPhase::Closed,
        SessionPhase::PreOpen,
        SessionPhase::Break,
        SessionPhase::Unknown,
        SessionPhase::Open,
        SessionPhase::Extended,
        SessionPhase::AlwaysOpen,
    ] {
        let directory = TestDirectory::new(&format!("session-{phase:?}"));
        let (service, _clock) = feed_service(&directory);
        let feed = service
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .unwrap();
        feed.push(MarketFeedEvent::Terms(terms())).unwrap();
        feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
            .unwrap();
        feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
            .unwrap();
        let MarketFeedEvent::Session(mut session) = live_session(route().instrument_id, 1) else {
            unreachable!()
        };
        session.phase = phase;
        session.next_open_unix_nanos = Some(2_000_000_000_000);
        feed.push(MarketFeedEvent::Session(session)).unwrap();
        service.status().unwrap();
        let baseline = service.snapshot().unwrap().revision;
        for (index, order_type) in [
            OrderType::Market,
            OrderType::Limit,
            OrderType::Stop,
            OrderType::StopLimit,
        ]
        .into_iter()
        .enumerate()
        {
            let mut order = market_order(
                &format!("session-{phase:?}-{index}"),
                OrderSide::Buy,
                u64::try_from(index + 2).unwrap(),
                1_000,
            );
            order.order_type = order_type;
            order.limit_price = matches!(order_type, OrderType::Limit | OrderType::StopLimit)
                .then(|| FixedPoint::try_new(9_000, 2).unwrap());
            order.stop_price = matches!(order_type, OrderType::Stop | OrderType::StopLimit)
                .then(|| FixedPoint::try_new(10_100, 2).unwrap());
            let result = service.place_order(order);
            match phase {
                SessionPhase::Unknown => assert_eq!(result.unwrap_err(), "market session unknown"),
                SessionPhase::Closed | SessionPhase::PreOpen | SessionPhase::Break => {
                    let rejection = result.unwrap_err();
                    assert!(rejection.contains("market closed"), "{rejection}");
                    assert!(rejection.contains(&format!("{phase:?}")), "{rejection}");
                }
                _ => assert!(result.is_ok(), "{phase:?} {order_type:?}: {result:?}"),
            }
        }
        let bracket = service.place_inline_bracket(PlaceInlineBracket {
            entry: market_order(&format!("bracket-{phase:?}"), OrderSide::Buy, 7, 1_000),
            template: super::fixed_stop_bracket_template(),
        });
        match phase {
            SessionPhase::Unknown => assert_eq!(bracket.unwrap_err(), "market session unknown"),
            SessionPhase::Closed | SessionPhase::PreOpen | SessionPhase::Break => {
                assert!(bracket.unwrap_err().contains("market closed"));
            }
            _ => assert!(bracket.is_ok(), "{phase:?}: {bracket:?}"),
        }
        let after = service.snapshot().unwrap();
        if matches!(
            phase,
            SessionPhase::Unknown
                | SessionPhase::Closed
                | SessionPhase::PreOpen
                | SessionPhase::Break
        ) {
            assert_eq!(after.revision, baseline);
            assert_eq!(after.orders, []);
            assert_eq!(after.order_events, []);
        } else {
            assert_eq!(after.orders.len(), 5);
        }
        service.shutdown(Duration::from_secs(2)).unwrap();
    }
}

#[test]
fn provider_families_use_the_same_verified_session_gate() {
    for provider in ["hyperliquid", "ctrader", "tastytrade", "rithmic"] {
        let directory = TestDirectory::new(&format!("session-provider-{provider}"));
        let (service, _clock) = feed_service(&directory);
        let feed = service
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .unwrap();
        let mut verified = terms();
        verified.route.provider_id = provider.into();
        let mut current_quote = quote(1, 1, 10_000, 10_025);
        current_quote.provider_id = provider.into();
        feed.push(MarketFeedEvent::Terms(verified)).unwrap();
        feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
            .unwrap();
        feed.push(MarketFeedEvent::Quote(current_quote)).unwrap();
        let MarketFeedEvent::Session(mut session) = live_session(route().instrument_id, 1) else {
            unreachable!()
        };
        session.phase = SessionPhase::Closed;
        session.next_open_unix_nanos = Some(2_000_000_000_000);
        feed.push(MarketFeedEvent::Session(session)).unwrap();
        service.status().unwrap();
        let before = service.snapshot().unwrap();
        assert!(
            service
                .place_order(market_order(provider, OrderSide::Buy, 2, 1_000))
                .unwrap_err()
                .contains("market closed"),
            "{provider}"
        );
        let after = service.snapshot().unwrap();
        assert_eq!(after.revision, before.revision, "{provider}");
        assert_eq!(after.orders, before.orders, "{provider}");
        assert_eq!(after.order_events, before.order_events, "{provider}");
        service.shutdown(Duration::from_secs(2)).unwrap();
    }
}

#[test]
fn unavailable_and_stale_feeds_reject_without_durable_orders() {
    let directory = TestDirectory::new("feed-unavailable");
    let (service, _clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .unwrap();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    service.status().unwrap();
    for (index, state) in [
        None,
        Some(FeedState::Stale {
            reason: FeedReason::Disconnected,
        }),
        Some(FeedState::Unavailable {
            reason: FeedReason::Unsupported,
        }),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(state) = state {
            feed.push(feed_state(route().instrument_id, 1, state))
                .unwrap();
            service.status().unwrap();
        }
        assert_eq!(
            service
                .place_order(market_order(
                    &format!("unavailable-{index}"),
                    OrderSide::Buy,
                    2,
                    1_000
                ))
                .unwrap_err(),
            "live market quote unavailable"
        );
    }
    assert_eq!(service.snapshot().unwrap().orders, []);
    service.shutdown(Duration::from_secs(2)).unwrap();
}

#[test]
fn manual_clock_enforces_per_class_quote_age_on_order_admission() {
    for (class, limit_ms) in [
        (
            aeris_instruments::InstrumentClass::CryptoPerpetual,
            10_000_i64,
        ),
        (aeris_instruments::InstrumentClass::Future, 60_000),
    ] {
        let directory = TestDirectory::new(&format!("age-{class:?}"));
        let (service, clock) = feed_service(&directory);
        let feed = service
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .unwrap();
        let mut verified = terms();
        verified.instrument_class = class;
        feed.push(MarketFeedEvent::Terms(verified)).unwrap();
        feed.push(live_session(route().instrument_id, 1)).unwrap();
        feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
            .unwrap();
        feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
            .unwrap();
        service.status().unwrap();
        clock.set_wall_unix_nanos(1_000_001_000_000 + limit_ms * 1_000_000);
        let mut at_limit = market_order("at-limit", OrderSide::Buy, 2, 1_000);
        at_limit.order_type = OrderType::Limit;
        at_limit.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
        service.place_order(at_limit).unwrap();
        clock.advance(Duration::from_millis(1));
        let mut expired = market_order("past-limit", OrderSide::Buy, 3, 1_000);
        expired.order_type = OrderType::Limit;
        expired.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
        assert_eq!(
            service.place_order(expired).unwrap_err(),
            format!(
                "market quote stale ({} ms exceeds {limit_ms} ms)",
                limit_ms + 1
            )
        );
        clock.advance(Duration::from_millis(999));
        let mut older = market_order("well-past-limit", OrderSide::Buy, 4, 1_000);
        older.order_type = OrderType::Limit;
        older.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
        assert_eq!(
            service.place_order(older).unwrap_err(),
            format!(
                "market quote stale ({} ms exceeds {limit_ms} ms)",
                limit_ms + 1_000
            )
        );
        service.shutdown(Duration::from_secs(2)).unwrap();
    }
}

#[test]
fn closed_phase_updates_marks_but_holds_crossed_resting_order_until_live_open_quote() {
    let directory = TestDirectory::new("closed-resting");
    let (service, clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .unwrap();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    service.status().unwrap();
    service
        .place_order(market_order("open-long", OrderSide::Buy, 2, 1_000))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 2, 9_975, 10_000)))
        .unwrap();
    service.status().unwrap();
    let mut stop = market_order("closed-stop", OrderSide::Sell, 3, 1_000);
    stop.order_type = OrderType::Stop;
    stop.time_in_force = TimeInForce::GoodTillCancelled;
    stop.stop_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    service.place_order(stop).unwrap();
    let initial_mark = service.snapshot().unwrap().positions[0].unrealized_pnl;
    let MarketFeedEvent::Session(mut closed) = live_session(route().instrument_id, 1) else {
        unreachable!()
    };
    closed.phase = SessionPhase::Closed;
    feed.push(MarketFeedEvent::Session(closed)).unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 3, 9_800, 9_825)))
        .unwrap();
    service.status().unwrap();
    let held = service.snapshot().unwrap();
    assert_eq!(held.fills.len(), 1);
    assert_ne!(held.positions[0].unrealized_pnl, initial_mark);
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(feed_state(
        route().instrument_id,
        1,
        FeedState::Stale {
            reason: FeedReason::Recovering,
        },
    ))
    .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 4, 9_775, 9_800)))
        .unwrap();
    service.status().unwrap();
    assert_eq!(service.snapshot().unwrap().fills.len(), 1);
    feed.push(feed_state(route().instrument_id, 2, FeedState::Live))
        .unwrap();
    feed.push(live_session(route().instrument_id, 2)).unwrap();
    clock.advance(Duration::from_millis(5));
    feed.push(MarketFeedEvent::Quote(quote(1, 5, 9_750, 9_775)))
        .unwrap();
    service.status().unwrap();
    assert_eq!(
        service.snapshot().unwrap().fills.len(),
        1,
        "retired generation"
    );
    feed.push(MarketFeedEvent::Quote(quote(2, 6, 9_700, 9_725)))
        .unwrap();
    service.status().unwrap();
    assert_eq!(service.snapshot().unwrap().fills.len(), 2);
    service.shutdown(Duration::from_secs(2)).unwrap();
}

#[test]
fn closed_session_rejects_practice_order_without_writing_a_row() {
    let directory = TestDirectory::new("closed-session-gate");
    let service = start_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink).unwrap();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
    feed.push(MarketFeedEvent::Session(
        aeris_trading::market::InstrumentSession {
            instrument_id: route().instrument_id,
            provider_generation: 1,
            phase: SessionPhase::Closed,
            source: CalendarSourceKind::ProviderCalendar,
            session_end_unix_nanos: None,
            next_open_unix_nanos: Some(2_000_000_000_000),
        },
    ))
    .unwrap();
    feed.push(MarketFeedEvent::Feed {
        instrument_id: route().instrument_id,
        state: FeedState::Live,
        provider_generation: 1,
    })
    .unwrap();
    service.status().unwrap();
    let before = service.snapshot().unwrap();
    let result = service.place_order(market_order("closed-market", OrderSide::Buy, 1, 1_000));
    assert!(result.unwrap_err().contains("market closed"));
    let after = service.snapshot().unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.orders, before.orders);
    service.shutdown(Duration::from_secs(2)).unwrap();
}

#[derive(Default)]
struct RecordedInterest(Mutex<Vec<QuoteInterest>>);

impl QuoteInterestSink for RecordedInterest {
    fn set_interest(&self, interest: QuoteInterest) -> Result<(), MarketFeedError> {
        self.0.lock().unwrap().push(interest);
        Ok(())
    }
}

#[test]
fn quote_book_validates_and_fences_generation_and_source_sequence() {
    let mut book = crate::core::market::feed::QuoteBook::default();
    let routes = [(route().instrument_id.clone(), route())].into();
    book.set_generation_floor(&route().instrument_id, 2);
    assert!(
        !book
            .accept(
                MarketFeedEvent::Quote(quote(1, 99, 10_000, 10_025)),
                &routes
            )
            .unwrap()
    );
    assert!(
        book.accept(MarketFeedEvent::Quote(quote(2, 4, 10_000, 10_025)), &routes)
            .unwrap()
    );
    for invalid in [quote(3, 5, 0, 10_025), quote(3, 5, 10_025, 10_025)] {
        assert_eq!(
            book.accept(MarketFeedEvent::Quote(invalid), &routes),
            Err(MarketFeedError::InvalidSpread)
        );
    }
    let mut wrong_scale = quote(3, 5, 10_000, 10_025);
    wrong_scale.bid = FixedPoint::try_new(10_000, 3).unwrap();
    assert_eq!(
        book.accept(MarketFeedEvent::Quote(wrong_scale), &routes),
        Err(MarketFeedError::InvalidScale)
    );
    assert!(
        !book
            .accept(MarketFeedEvent::Quote(quote(2, 3, 9_900, 10_000)), &routes)
            .unwrap()
    );
    assert_eq!(
        book.quote(&route().instrument_id).unwrap().bid.units(),
        10_000
    );
    assert!(
        book.accept(MarketFeedEvent::Quote(quote(3, 1, 9_900, 10_000)), &routes)
            .unwrap()
    );
    assert_eq!(
        book.quote(&route().instrument_id).unwrap().bid.units(),
        9_900
    );
}

#[test]
fn owner_prices_chart_bracket_entry_against_its_current_touch() {
    let price = |units| FixedPoint::try_new(units, 2).unwrap();
    let (bid, ask) = (price(3_088_400), price(3_088_425));
    for (side, entry, expected) in [
        (OrderSide::Buy, 3_108_950, OrderType::Stop),
        (OrderSide::Buy, 3_088_425, OrderType::Limit),
        (OrderSide::Buy, 3_080_000, OrderType::Limit),
        (OrderSide::Sell, 3_080_000, OrderType::Stop),
        (OrderSide::Sell, 3_088_400, OrderType::Limit),
        (OrderSide::Sell, 3_108_950, OrderType::Limit),
    ] {
        assert_eq!(
            crate::core::resting_bracket_order_type(side, price(entry), bid, ask),
            Ok(expected),
            "{side:?} at {entry}"
        );
    }
    let coarse = FixedPoint::try_new(30_884, 0).unwrap();
    assert!(
        crate::core::resting_bracket_order_type(OrderSide::Buy, coarse, bid, price(3_088_425))
            .is_err(),
        "an inexact quote cannot classify the bracket entry"
    );
}

#[test]
fn recovered_feed_needs_a_new_quote_before_execution() {
    let mut book = crate::core::market::feed::QuoteBook::default();
    let id = route().instrument_id;
    let routes = [(id.clone(), route())].into();
    let feed = |state| MarketFeedEvent::Feed {
        instrument_id: id.clone(),
        state,
        provider_generation: 1,
    };
    book.accept(feed(FeedState::Live), &routes).unwrap();
    book.accept(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)), &routes)
        .unwrap();
    assert!(book.observation(&id).is_some());
    book.accept(
        feed(FeedState::Stale {
            reason: FeedReason::Disconnected,
        }),
        &routes,
    )
    .unwrap();
    book.accept(MarketFeedEvent::Quote(quote(1, 2, 9_900, 9_925)), &routes)
        .unwrap();
    assert!(book.observation(&id).is_none());
    book.accept(feed(FeedState::Live), &routes).unwrap();
    assert!(book.observation(&id).is_none());
    book.accept(MarketFeedEvent::Quote(quote(1, 3, 9_875, 9_900)), &routes)
        .unwrap();
    assert_eq!(book.observation(&id).unwrap().bid.units(), 9_875);
}

#[test]
fn feed_inbox_latest_wins_and_control_overflow_requests_resync() {
    let (commands, receiver) = mpsc::sync_channel(2);
    let inbox =
        crate::core::market::feed::MarketFeedInbox::new(commands, Arc::new(AtomicBool::new(false)));
    inbox
        .push(MarketFeedEvent::Quote(quote(2, 3, 10_000, 10_025)))
        .unwrap();
    inbox
        .push(MarketFeedEvent::Quote(quote(2, 2, 9_900, 10_000)))
        .unwrap();
    assert!(matches!(receiver.try_recv(), Ok(Command::DrainMarketFeed)));
    assert!(receiver.try_recv().is_err());
    for _ in 0..1_025 {
        inbox.push(MarketFeedEvent::Terms(terms())).unwrap();
    }
    let batch = inbox.drain();
    assert_eq!(batch.quotes.len(), 1);
    assert_eq!(batch.quotes[0].source_sequence, Some(3));
    assert_eq!(batch.controls.len(), 1_024);
    assert!(batch.resync);
}

#[test]
fn verified_terms_persist_routes_and_recovered_orders_republish_interest() {
    let directory = TestDirectory::new("verified-market-route");
    let (service, clock) = feed_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink.clone()).unwrap();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(MarketFeedEvent::Feed {
        instrument_id: route().instrument_id,
        state: FeedState::Live,
        provider_generation: 1,
    })
    .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    service.status().unwrap();
    let mut order = market_order("resting-without-pane", OrderSide::Buy, 1, 1_000);
    order.order_type = OrderType::Limit;
    order.time_in_force = TimeInForce::GoodTillCancelled;
    order.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
    service.place_order(order).unwrap();
    service.status().unwrap();
    assert_eq!(sink.0.lock().unwrap().last().unwrap().routes, vec![route()]);
    service
        .cancel_order(ClientOrderId::try_new("resting-without-pane").unwrap())
        .unwrap();
    service.status().unwrap();
    assert_eq!(sink.0.lock().unwrap().last().unwrap().routes, Vec::new());
    service.set_ticket_interest(vec![route()]).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(MarketFeedEvent::Feed {
        instrument_id: route().instrument_id,
        state: FeedState::Live,
        provider_generation: 1,
    })
    .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 2, 10_000, 10_025)))
        .unwrap();
    service.status().unwrap();
    let mut order = market_order("recover-without-pane", OrderSide::Buy, 2, 2_000);
    order.order_type = OrderType::Limit;
    order.time_in_force = TimeInForce::GoodTillCancelled;
    order.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
    service.place_order(order).unwrap();
    service.set_ticket_interest(Vec::new()).unwrap();
    service.shutdown(Duration::from_secs(2)).unwrap();

    let connection = rusqlite::Connection::open(config(&directory).database_path).unwrap();
    let route_json: String = connection
        .query_row(
            "SELECT market_route_json FROM instruments WHERE id = ?1",
            [route().instrument_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&route_json).unwrap()["provider_symbol"],
        "ESZ6"
    );
    drop(connection);

    clock.set_wall_unix_nanos(1_000_000_010_000);
    let mut configuration = config(&directory);
    configuration.clock = clock;
    let restarted = TradingService::start(configuration).unwrap();
    let recovered = Arc::new(RecordedInterest::default());
    let feed = restarted.attach_market_feed(recovered.clone()).unwrap();
    assert_eq!(recovered.0.lock().unwrap()[0].routes, vec![route()]);
    feed.push(MarketFeedEvent::Feed {
        instrument_id: route().instrument_id,
        state: FeedState::Stale {
            reason: FeedReason::Recovering,
        },
        provider_generation: 2,
    })
    .unwrap();
    restarted.status().unwrap();
    restarted.shutdown(Duration::from_secs(2)).unwrap();
}

#[test]
fn pane_free_quotes_fill_stops_update_marks_and_survive_restart() {
    let directory = TestDirectory::new("position-feed-interest");
    let (service, clock) = feed_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink.clone()).unwrap();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(MarketFeedEvent::Feed {
        instrument_id: route().instrument_id,
        state: FeedState::Live,
        provider_generation: 1,
    })
    .unwrap();
    service.status().unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    service.status().unwrap();
    service
        .place_order_from_market(
            market_order("position-open", OrderSide::Buy, 1, 1_000),
            false,
        )
        .unwrap();
    service.status().unwrap();
    assert_eq!(sink.0.lock().unwrap().last().unwrap().routes, vec![route()]);
    feed.push(MarketFeedEvent::Quote(quote(1, 2, 9_975, 10_000)))
        .unwrap();
    service.status().unwrap();
    assert_eq!(
        service.snapshot().unwrap().positions[0]
            .net_quantity
            .units(),
        1
    );
    let mut stop = market_order("pane-free-stop", OrderSide::Sell, 3, 3_000);
    stop.order_type = OrderType::Stop;
    stop.time_in_force = TimeInForce::GoodTillCancelled;
    stop.stop_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    service.place_order_from_market(stop, false).unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 4, 9_950, 9_975)))
        .unwrap();
    service.status().unwrap();
    let mark = service.snapshot().unwrap().positions[0].unrealized_pnl;
    feed.push(MarketFeedEvent::Quote(quote(1, 5, 9_925, 9_950)))
        .unwrap();
    service.status().unwrap();
    assert_ne!(
        service.snapshot().unwrap().positions[0].unrealized_pnl,
        mark
    );
    assert_eq!(sink.0.lock().unwrap().last().unwrap().routes, vec![route()]);
    service.shutdown(Duration::from_secs(2)).unwrap();
    clock.set_wall_unix_nanos(1_000_000_000_000 + 5_500_000);
    let mut configuration = config(&directory);
    configuration.clock = clock;
    let restarted = TradingService::start(configuration).unwrap();
    let sink = Arc::new(RecordedInterest::default());
    let feed = restarted.attach_market_feed(sink.clone()).unwrap();
    assert_eq!(sink.0.lock().unwrap()[0].routes, vec![route()]);
    feed.push(live_session(route().instrument_id, 2)).unwrap();
    feed.push(MarketFeedEvent::Feed {
        instrument_id: route().instrument_id,
        state: FeedState::Live,
        provider_generation: 2,
    })
    .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(2, 5, 9_875, 9_900)))
        .unwrap();
    restarted.status().unwrap();
    assert_eq!(
        restarted.snapshot().unwrap().positions[0]
            .net_quantity
            .units(),
        1,
        "a crossed pre-restart quote cannot fill a restored order"
    );
    feed.push(MarketFeedEvent::Quote(quote(2, 6, 9_875, 9_900)))
        .unwrap();
    restarted.status().unwrap();
    let snapshot = restarted.snapshot().unwrap();
    assert_eq!(snapshot.positions[0].net_quantity.units(), 0);
    assert_eq!(snapshot.fills.len(), 2);
    assert_eq!(sink.0.lock().unwrap().last().unwrap().routes, Vec::new());
    restarted.shutdown(Duration::from_secs(2)).unwrap();
}

#[test]
fn ticket_interest_is_bounded_and_unchanged_sets_are_not_republished() {
    let directory = TestDirectory::new("ticket-interest");
    let service = start_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    service.attach_market_feed(sink.clone()).unwrap();
    service.set_ticket_interest(vec![route()]).unwrap();
    service.set_ticket_interest(vec![route()]).unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert_eq!(
        service.set_ticket_interest(vec![route(); 65]).unwrap_err(),
        "ticket quote interest limit reached"
    );
    service.set_ticket_interest(Vec::new()).unwrap();
    service.status().unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 2);
    service.shutdown(Duration::from_secs(2)).unwrap();
}

#[test]
fn admission_refuses_the_257th_routed_instrument_before_persisting() {
    let directory = TestDirectory::new("interest-capacity");
    let (service, _clock) = feed_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink.clone()).unwrap();
    for index in 0_u64..=256 {
        let mut terms = terms();
        terms.route.instrument_id =
            InstrumentId::try_new(format!("instrument:fixture:{index}")).unwrap();
        feed.push(MarketFeedEvent::Terms(terms.clone())).unwrap();
        feed.push(live_session(terms.route.instrument_id.clone(), 1))
            .unwrap();
        feed.push(MarketFeedEvent::Feed {
            instrument_id: terms.route.instrument_id.clone(),
            state: FeedState::Live,
            provider_generation: 1,
        })
        .unwrap();
        let mut price = quote(1, 1, 10_000, 10_025);
        price.instrument_id = terms.route.instrument_id.clone();
        feed.push(MarketFeedEvent::Quote(price)).unwrap();
        service.status().unwrap();
        let mut order = market_order(
            &format!("interest-{index}"),
            OrderSide::Buy,
            index + 1,
            1_000,
        );
        order.instrument_id = terms.route.instrument_id;
        order.order_type = OrderType::Limit;
        order.time_in_force = TimeInForce::GoodTillCancelled;
        order.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
        if index == 256 {
            assert_eq!(
                service.place_order(order).unwrap_err(),
                "trading quote interest limit reached"
            );
        } else {
            service.place_order(order).unwrap();
        }
        service.status().unwrap();
    }
    let history = sink.0.lock().unwrap();
    assert_eq!(history.last().unwrap().routes.len(), 256);
    assert!(history.iter().all(|interest| interest.routes.len() <= 256));
    drop(history);
    assert_eq!(service.snapshot().unwrap().orders.len(), 256);
    service.shutdown(Duration::from_secs(2)).unwrap();
}
