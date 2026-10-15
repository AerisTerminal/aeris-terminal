use super::*;
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
        received_unix_nanos: i64::try_from(sequence).unwrap(),
    }
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
    let service = start_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink.clone()).unwrap();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
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
    let mut order = market_order("recover-without-pane", OrderSide::Buy, 2, 2_000);
    order.order_type = OrderType::Limit;
    order.time_in_force = TimeInForce::GoodTillCancelled;
    order.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
    service.place_order(order).unwrap();
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

    let restarted = TradingService::start(config(&directory)).unwrap();
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
fn position_keeps_interest_after_fill_and_feed_quotes_cannot_execute_orders_yet() {
    let directory = TestDirectory::new("position-feed-interest");
    let service = start_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink.clone()).unwrap();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
    service.status().unwrap();
    let mut buy = market_order("position-open", OrderSide::Buy, 1, 1_000);
    buy.order_type = OrderType::Limit;
    buy.time_in_force = TimeInForce::GoodTillCancelled;
    buy.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
    service.place_order(buy).unwrap();
    service.status().unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 2, 8_900, 8_950)))
        .unwrap();
    service.status().unwrap();
    assert!(
        service.snapshot().unwrap().fills.is_empty(),
        "feed does not drive execution until cutover"
    );
    // The legacy simulation entry point remains the sole execution authority in part 2.
    service
        .observe_market(observation(8_900, 8_950, 3, 3_000))
        .unwrap();
    service.status().unwrap();
    assert_eq!(
        service.snapshot().unwrap().positions[0]
            .net_quantity
            .units(),
        1
    );
    assert_eq!(sink.0.lock().unwrap().last().unwrap().routes, vec![route()]);
    service.shutdown(Duration::from_secs(2)).unwrap();
    let restarted = TradingService::start(config(&directory)).unwrap();
    let sink = Arc::new(RecordedInterest::default());
    restarted.attach_market_feed(sink.clone()).unwrap();
    assert_eq!(sink.0.lock().unwrap()[0].routes, vec![route()]);
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
    let service = start_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink.clone()).unwrap();
    for index in 0_u64..=256 {
        let mut terms = terms();
        terms.route.instrument_id =
            InstrumentId::try_new(format!("instrument:fixture:{index}")).unwrap();
        feed.push(MarketFeedEvent::Terms(terms.clone())).unwrap();
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
