use super::*;
use aeris_instruments::{CalendarSourceKind, SessionPhase};
use aeris_trading::market::{
    FeedReason, FeedState, InstrumentTerms, MarketFeedError, MarketFeedEvent, MarketRoute,
    QuoteInterest, QuoteInterestSink, TradingQuote,
};

pub(crate) fn route() -> MarketRoute {
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

pub(crate) fn terms() -> InstrumentTerms {
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

fn forex_weekly_terms() -> InstrumentTerms {
    let mut contract = terms();
    contract.session_hours = [
        (7, 17 * 3_600, 86_400),
        (1, 0, 86_400),
        (2, 0, 86_400),
        (3, 0, 86_400),
        (4, 0, 86_400),
        (5, 0, 17 * 3_600),
    ]
    .into_iter()
    .map(
        |(weekday, open_seconds, close_seconds)| aeris_instruments::SessionHours {
            weekday,
            open_seconds,
            close_seconds,
            timezone: "America/New_York".into(),
        },
    )
    .collect();
    contract
}

pub(crate) fn quote(generation: u64, sequence: u64, bid: i64, ask: i64) -> TradingQuote {
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

pub(crate) fn feed_service(
    directory: &TestDirectory,
) -> (TradingService, Arc<crate::clock::ManualClock>) {
    let clock = Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(directory);
    configuration.clock = clock.clone();
    (start_service_with_config(configuration), clock)
}

pub(crate) fn live_session(id: InstrumentId, generation: u64) -> MarketFeedEvent {
    MarketFeedEvent::Session(aeris_trading::market::InstrumentSession {
        instrument_id: id,
        provider_generation: generation,
        phase: SessionPhase::Open,
        source: CalendarSourceKind::ProviderCalendar,
        session_end_unix_nanos: Some(2_000_000_000_000),
        next_open_unix_nanos: None,
    })
}

#[test]
fn day_acceptance_fixes_session_deadline_and_restores_it() {
    let directory = TestDirectory::new("day-acceptance-deadline");
    let (service, clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .expect("feed");
    feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
    feed.push(live_session(route().instrument_id, 1))
        .expect("session");
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .expect("live");
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .expect("quote");
    service.status().expect("drain");
    let mut request = market_order("day-session", OrderSide::Buy, 1, 1_000_000_000_000);
    request.order_type = OrderType::Limit;
    request.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    request.time_in_force = TimeInForce::Day;
    let day = service.place_order(request).expect("day accepted");
    assert_eq!(day.status, OrderStatus::Working);
    assert_eq!(day.expires_at_unix_nanos, Some(2_000_000_000_000));
    let mut gtc = market_order("gtc-session", OrderSide::Buy, 1, 1_000_000_000_000);
    gtc.order_type = OrderType::Limit;
    gtc.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    gtc.time_in_force = TimeInForce::GoodTillCancelled;
    assert_eq!(
        service.place_order(gtc).unwrap().expires_at_unix_nanos,
        None
    );
    let before = service.snapshot().unwrap();
    service.shutdown(Duration::from_secs(5)).unwrap();
    let mut configuration = config(&directory);
    configuration.clock = clock;
    let restarted = TradingService::start(configuration).unwrap();
    assert_eq!(restarted.snapshot().unwrap().orders, before.orders);
    restarted.shutdown(Duration::from_secs(5)).unwrap();
}

#[test]
fn day_order_expires_on_owner_boundary_before_late_quote() {
    let directory = TestDirectory::new("day-owner-expiry");
    let (service, clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .expect("feed");
    feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
    feed.push(live_session(route().instrument_id, 1))
        .expect("session");
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .expect("live");
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .expect("quote");
    service.status().expect("drain");
    let mut request = market_order("day-expiry", OrderSide::Buy, 1, 1_000_000_000_000);
    request.order_type = OrderType::Limit;
    request.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    request.time_in_force = TimeInForce::Day;
    let accepted = service.place_order(request).expect("day accepted");
    clock.set_wall_unix_nanos(1_999_999_000_000);
    assert_eq!(
        service
            .snapshot()
            .unwrap()
            .orders
            .iter()
            .find(|order| order.id == accepted.id)
            .unwrap()
            .status,
        OrderStatus::Working
    );
    clock.set_wall_unix_nanos(2_000_000_000_000);
    let snapshot = service.snapshot().expect("deadline precedes snapshot");
    let expired = snapshot
        .orders
        .iter()
        .find(|order| order.id == accepted.id)
        .expect("expired order");
    assert_eq!(expired.status, OrderStatus::Expired);
    assert_eq!(expired.terminal_unix_nanos, Some(2_000_000_000_000));
    assert_eq!(expired.expires_at_unix_nanos, Some(2_000_000_000_000));
    assert_eq!(expired.revision, accepted.revision + 1);
    assert!(snapshot.order_events.iter().any(|event| {
        event.order_id == accepted.id
            && event.kind == OrderEventKind::Expired
            && event.event_unix_nanos == 2_000_000_000_000
    }));
    feed.push(MarketFeedEvent::Quote(quote(1, 2, 10_000, 10_025)))
        .expect("late quote");
    assert_eq!(service.snapshot().unwrap().fills.len(), 0);
    service.shutdown(Duration::from_secs(5)).unwrap();
    let mut configuration = config(&directory);
    configuration.clock = clock;
    let restarted = start_service_with_config(configuration);
    assert_eq!(
        restarted
            .snapshot()
            .unwrap()
            .orders
            .iter()
            .find(|order| order.id == accepted.id),
        Some(expired)
    );
    restarted.shutdown(Duration::from_secs(5)).unwrap();
}

#[test]
fn overdue_day_order_is_expired_before_first_command_after_restart() {
    let directory = TestDirectory::new("day-restart-expiry");
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
    let mut request = market_order("day-restart", OrderSide::Buy, 1, 1_000_000_000_000);
    request.order_type = OrderType::Limit;
    request.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    request.time_in_force = TimeInForce::Day;
    let working = service.place_order(request).unwrap();
    assert_eq!(working.status, OrderStatus::Working);
    service.shutdown(Duration::from_secs(5)).unwrap();

    clock.set_wall_unix_nanos(2_000_001_000_000);
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let (commands, _receiver) = std::sync::mpsc::sync_channel(1);
    let recovered_core = TradingCore::open(
        &configuration.database_path,
        configuration.retention,
        configuration.clock.clone(),
        configuration.execution,
        std::sync::Weak::new(),
        Arc::new(crate::core::market::feed::MarketFeedInbox::new(
            commands,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )),
    )
    .unwrap();
    let status: String = rusqlite::Connection::open(&configuration.database_path)
        .unwrap()
        .query_row(
            "SELECT status FROM orders WHERE id = ?1",
            [working.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "expired",
        "store-open recovery precedes the service loop"
    );
    drop(recovered_core);
    let restarted = start_service_with_config(configuration);
    let snapshot = restarted.snapshot().expect("first command after recovery");
    let expired = snapshot
        .orders
        .iter()
        .find(|order| order.id == working.id)
        .unwrap();
    assert_eq!(expired.status, OrderStatus::Expired);
    assert_eq!(expired.expires_at_unix_nanos, Some(2_000_000_000_000));
    assert_eq!(expired.terminal_unix_nanos, Some(2_000_000_000_000));
    assert_eq!(expired.revision, working.revision + 1);
    assert!(snapshot.order_events.iter().any(|event| {
        event.order_id == working.id
            && event.kind == OrderEventKind::Expired
            && event.event_unix_nanos == 2_000_000_000_000
    }));
    restarted.shutdown(Duration::from_secs(5)).unwrap();
}

#[test]
fn migrated_day_without_deadline_cannot_fill_before_calendar_recovery() {
    let directory = TestDirectory::new("day-migrated-null-fence");
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
    let mut request = market_order("migrated-day", OrderSide::Buy, 1, 1_000_000_000_000);
    request.order_type = OrderType::Limit;
    request.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    request.time_in_force = TimeInForce::Day;
    let working = service.place_order(request).unwrap();
    service.shutdown(Duration::from_secs(5)).unwrap();

    let configuration = config(&directory);
    rusqlite::Connection::open(&configuration.database_path)
        .unwrap()
        .execute(
            "UPDATE orders SET expires_at_unix_nanos = NULL WHERE id = ?1",
            [working.id.as_str()],
        )
        .unwrap();
    for generation in 2..=3 {
        let mut configuration = config(&directory);
        configuration.clock = clock.clone();
        let restarted = start_service_with_config(configuration);
        let feed = restarted
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .unwrap();
        feed.push(MarketFeedEvent::Terms(terms())).unwrap();
        feed.push(live_session(route().instrument_id, generation))
            .unwrap();
        feed.push(feed_state(
            route().instrument_id,
            generation,
            FeedState::Live,
        ))
        .unwrap();
        feed.push(MarketFeedEvent::Quote(quote(
            generation, generation, 9_800, 9_850,
        )))
        .unwrap();
        let snapshot = restarted.snapshot().unwrap();
        assert!(snapshot.fills.is_empty(), "unresolved DAY order filled");
        assert_eq!(
            snapshot
                .orders
                .iter()
                .find(|order| order.id == working.id)
                .unwrap()
                .status,
            OrderStatus::Working
        );
        restarted.shutdown(Duration::from_secs(5)).unwrap();
    }
}

#[test]
fn migrated_always_open_day_recovers_acceptance_midnight_before_first_command() {
    let directory = TestDirectory::new("day-migrated-continuous");
    let (service, clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .unwrap();
    let mut continuous = terms();
    continuous.route.venue_id = "HYPERLIQUID".into();
    continuous.instrument_class = aeris_instruments::InstrumentClass::CryptoPerpetual;
    continuous.session_hours.clear();
    feed.push(MarketFeedEvent::Terms(continuous)).unwrap();
    feed.push(MarketFeedEvent::Session(
        aeris_trading::market::InstrumentSession {
            instrument_id: route().instrument_id,
            provider_generation: 1,
            phase: SessionPhase::AlwaysOpen,
            source: CalendarSourceKind::AlwaysOpen,
            session_end_unix_nanos: None,
            next_open_unix_nanos: None,
        },
    ))
    .unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    service.status().unwrap();
    let mut request = market_order("migrated-continuous", OrderSide::Buy, 1, 1_000_000_000_000);
    request.order_type = OrderType::Limit;
    request.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    request.time_in_force = TimeInForce::Day;
    let working = service.place_order(request).unwrap();
    let acceptance_midnight = working.expires_at_unix_nanos.unwrap();
    service.shutdown(Duration::from_secs(5)).unwrap();

    let mut configuration = config(&directory);
    rusqlite::Connection::open(&configuration.database_path)
        .unwrap()
        .execute(
            "UPDATE orders SET expires_at_unix_nanos = NULL WHERE id = ?1",
            [working.id.as_str()],
        )
        .unwrap();
    clock.set_wall_unix_nanos(acceptance_midnight + 1);
    configuration.clock = clock;
    let recovered = start_service_with_config(configuration);
    let snapshot = recovered.snapshot().unwrap();
    let order = snapshot
        .orders
        .iter()
        .find(|order| order.id == working.id)
        .unwrap();
    assert_eq!(order.expires_at_unix_nanos, Some(acceptance_midnight));
    assert_eq!(order.status, OrderStatus::Expired);
    assert_eq!(order.terminal_unix_nanos, Some(acceptance_midnight));
    recovered.shutdown(Duration::from_secs(5)).unwrap();
}

#[test]
fn route_less_legacy_day_waits_for_verified_terms_before_expiring() {
    let directory = TestDirectory::new("day-route-less-late-terms");
    let (service, clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .unwrap();
    let mut continuous = terms();
    continuous.route.venue_id = "HYPERLIQUID".into();
    continuous.session_hours.clear();
    feed.push(MarketFeedEvent::Terms(continuous.clone()))
        .unwrap();
    feed.push(MarketFeedEvent::Session(
        aeris_trading::market::InstrumentSession {
            instrument_id: route().instrument_id,
            provider_generation: 1,
            phase: SessionPhase::AlwaysOpen,
            source: CalendarSourceKind::AlwaysOpen,
            session_end_unix_nanos: None,
            next_open_unix_nanos: None,
        },
    ))
    .unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    service.status().unwrap();
    let mut request = market_order("late-terms-day", OrderSide::Buy, 1, 1_000_000_000_000);
    request.order_type = OrderType::Limit;
    request.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    request.time_in_force = TimeInForce::Day;
    let placed = service.place_order(request).unwrap();
    let deadline = placed.expires_at_unix_nanos.unwrap();
    service.shutdown(Duration::from_secs(5)).unwrap();

    let mut configuration = config(&directory);
    let connection = rusqlite::Connection::open(&configuration.database_path).unwrap();
    connection
        .execute(
            "UPDATE orders SET expires_at_unix_nanos = NULL WHERE id = ?1",
            [placed.id.as_str()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE instruments SET market_route_json = NULL WHERE id = ?1",
            [route().instrument_id.as_str()],
        )
        .unwrap();
    clock.set_wall_unix_nanos(deadline + 1);
    configuration.clock = clock;
    let restarted = start_service_with_config(configuration);
    let before = restarted.snapshot().unwrap();
    let unresolved = before
        .orders
        .iter()
        .find(|order| order.id == placed.id)
        .unwrap();
    assert_eq!(unresolved.status, OrderStatus::Working);
    assert_eq!(unresolved.expires_at_unix_nanos, None);
    let feed = restarted
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .unwrap();
    feed.push(MarketFeedEvent::Terms(continuous)).unwrap();
    let after = restarted.snapshot().unwrap();
    let recovered = after
        .orders
        .iter()
        .find(|order| order.id == placed.id)
        .unwrap();
    assert_eq!(recovered.status, OrderStatus::Expired);
    assert_eq!(recovered.expires_at_unix_nanos, Some(deadline));
    assert_eq!(recovered.terminal_unix_nanos, Some(deadline));
    restarted.shutdown(Duration::from_secs(5)).unwrap();
}

#[test]
fn failed_multi_day_backfill_rolls_back_every_deadline_and_retries_on_restart() {
    let directory = TestDirectory::new("day-multi-backfill-atomic");
    let (service, clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .unwrap();
    let mut continuous = terms();
    continuous.route.venue_id = "HYPERLIQUID".into();
    continuous.session_hours.clear();
    feed.push(MarketFeedEvent::Terms(continuous)).unwrap();
    feed.push(MarketFeedEvent::Session(
        aeris_trading::market::InstrumentSession {
            instrument_id: route().instrument_id,
            provider_generation: 1,
            phase: SessionPhase::AlwaysOpen,
            source: CalendarSourceKind::AlwaysOpen,
            session_end_unix_nanos: None,
            next_open_unix_nanos: None,
        },
    ))
    .unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    service.status().unwrap();
    let mut request = market_order("day-batch-one", OrderSide::Buy, 1, 1_000_000_000_000);
    request.order_type = OrderType::Limit;
    request.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    request.time_in_force = TimeInForce::Day;
    let first = service.place_order(request.clone()).unwrap();
    request.client_order_id = ClientOrderId::try_new("day-batch-two").unwrap();
    let second = service.place_order(request).unwrap();
    let deadline = first.expires_at_unix_nanos.unwrap();
    assert_eq!(second.expires_at_unix_nanos, Some(deadline));
    service.shutdown(Duration::from_secs(5)).unwrap();

    let mut configuration = config(&directory);
    let connection = rusqlite::Connection::open(&configuration.database_path).unwrap();
    connection
        .execute(
            "UPDATE orders SET expires_at_unix_nanos = NULL WHERE id IN (?1, ?2)",
            [first.id.as_str(), second.id.as_str()],
        )
        .unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_second_day BEFORE UPDATE OF expires_at_unix_nanos ON orders
             WHEN NEW.id = '{}' BEGIN SELECT RAISE(ABORT, 'injected backfill failure'); END;",
            second.id.as_str()
        ))
        .unwrap();
    drop(connection);
    clock.set_wall_unix_nanos(deadline + 1);
    configuration.clock = clock.clone();
    assert!(
        TradingService::start(configuration).is_err(),
        "startup must refuse a partially backfilled DAY set"
    );
    let connection = rusqlite::Connection::open(config(&directory).database_path).unwrap();
    for id in [&first.id, &second.id] {
        let (deadline, status): (Option<i64>, String) = connection
            .query_row(
                "SELECT expires_at_unix_nanos, status FROM orders WHERE id = ?1",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(deadline, None);
        assert_eq!(status, "working");
    }
    connection
        .execute_batch("DROP TRIGGER fail_second_day")
        .unwrap();
    drop(connection);
    let mut configuration = config(&directory);
    configuration.clock = clock;
    let recovered = start_service_with_config(configuration);
    for id in [&first.id, &second.id] {
        let snapshot = recovered.snapshot().unwrap();
        let order = snapshot
            .orders
            .iter()
            .find(|order| &order.id == id)
            .unwrap();
        assert_eq!(order.status, OrderStatus::Expired);
        assert_eq!(order.expires_at_unix_nanos, Some(deadline));
        assert_eq!(order.terminal_unix_nanos, Some(deadline));
    }
    recovered.shutdown(Duration::from_secs(5)).unwrap();
}

#[test]
fn migrated_calendared_day_recovers_acceptance_session_not_restart_session() {
    const ACCEPTED: i64 = 1_793_806_200_000_000_000; // Wednesday 2026-11-04 15:30 UTC
    const CLOSE: i64 = 1_794_002_400_000_000_000; // Friday 17:00 New York, EST
    let directory = TestDirectory::new("day-migrated-weekly-dst");
    let clock = Arc::new(crate::clock::ManualClock::new(ACCEPTED));
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let service = start_service_with_config(configuration);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .unwrap();
    feed.push(MarketFeedEvent::Terms(forex_weekly_terms()))
        .unwrap();
    feed.push(MarketFeedEvent::Session(
        aeris_trading::market::InstrumentSession {
            instrument_id: route().instrument_id,
            provider_generation: 1,
            phase: SessionPhase::Open,
            source: CalendarSourceKind::ContractHours,
            session_end_unix_nanos: Some(CLOSE),
            next_open_unix_nanos: None,
        },
    ))
    .unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    let mut initial_quote = quote(1, 1, 10_000, 10_025);
    initial_quote.received_unix_nanos = ACCEPTED + 1_000_000;
    feed.push(MarketFeedEvent::Quote(initial_quote)).unwrap();
    service.status().unwrap();
    let mut day = market_order("migrated-weekly", OrderSide::Buy, 1, ACCEPTED);
    day.order_type = OrderType::Limit;
    day.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    day.time_in_force = TimeInForce::Day;
    let placed = service.place_order(day.clone()).unwrap();
    assert_eq!(placed.expires_at_unix_nanos, Some(CLOSE));
    day.client_order_id = ClientOrderId::try_new("weekly-gtc").unwrap();
    day.time_in_force = TimeInForce::GoodTillCancelled;
    let gtc = service.place_order(day).unwrap();
    service.shutdown(Duration::from_secs(5)).unwrap();
    let mut configuration = config(&directory);
    rusqlite::Connection::open(&configuration.database_path)
        .unwrap()
        .execute(
            "UPDATE orders SET expires_at_unix_nanos = NULL WHERE id = ?1",
            [placed.id.as_str()],
        )
        .unwrap();
    clock.set_wall_unix_nanos(CLOSE + 1);
    configuration.clock = clock.clone();
    let restarted = start_service_with_config(configuration);
    let snapshot = restarted.snapshot().unwrap();
    let restored = snapshot
        .orders
        .iter()
        .find(|order| order.id == placed.id)
        .unwrap();
    assert_eq!(restored.expires_at_unix_nanos, Some(CLOSE));
    assert_eq!(restored.status, OrderStatus::Expired);
    assert_eq!(restored.terminal_unix_nanos, Some(CLOSE));
    assert_eq!(
        snapshot
            .orders
            .iter()
            .find(|order| order.id == gtc.id)
            .unwrap()
            .status,
        OrderStatus::Working
    );
    restarted.shutdown(Duration::from_secs(5)).unwrap();
    // Friday closes on Nov 6, 13 and 20 all pass across the two restarts.
    clock.set_wall_unix_nanos(CLOSE + 14 * 86_400_000_000_000);
    let mut configuration = config(&directory);
    configuration.clock = clock;
    let restarted = start_service_with_config(configuration);
    let snapshot = restarted.snapshot().unwrap();
    assert_eq!(
        snapshot
            .orders
            .iter()
            .find(|order| order.id == gtc.id)
            .unwrap()
            .status,
        OrderStatus::Working
    );
    restarted.shutdown(Duration::from_secs(5)).unwrap();
}

pub(crate) fn feed_state(id: InstrumentId, generation: u64, state: FeedState) -> MarketFeedEvent {
    MarketFeedEvent::Feed {
        instrument_id: id,
        state,
        provider_generation: generation,
    }
}

#[test]
fn market_and_marketable_limit_fill_at_placement_touch_and_restore() {
    for (index, side, kind, tif, limit, expected) in [
        (
            0,
            OrderSide::Buy,
            OrderType::Market,
            TimeInForce::ImmediateOrCancel,
            None,
            10_025,
        ),
        (
            1,
            OrderSide::Sell,
            OrderType::Market,
            TimeInForce::ImmediateOrCancel,
            None,
            10_000,
        ),
        (
            2,
            OrderSide::Buy,
            OrderType::Limit,
            TimeInForce::GoodTillCancelled,
            Some(10_100),
            10_025,
        ),
        (
            3,
            OrderSide::Sell,
            OrderType::Limit,
            TimeInForce::ImmediateOrCancel,
            Some(9_900),
            10_000,
        ),
        (
            4,
            OrderSide::Buy,
            OrderType::Limit,
            TimeInForce::FillOrKill,
            Some(10_100),
            10_025,
        ),
    ] {
        let directory = TestDirectory::new(&format!("placement-touch-{index}"));
        let (service, _clock) = feed_service(&directory);
        service
            .set_ticket_interest(vec![route()])
            .expect("ticket interest");
        let feed = service
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .expect("feed");
        feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
        feed.push(live_session(route().instrument_id, 1))
            .expect("session");
        feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
            .expect("live");
        feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
            .expect("quote");
        service.status().expect("drain");
        let mut request = market_order(&format!("placement-{index}"), side, 1, 1_000_000_000_000);
        request.order_type = kind;
        request.time_in_force = tif;
        request.limit_price = limit.map(|units| FixedPoint::try_new(units, 2).expect("limit"));
        let order = service.place_order(request).expect("placement");
        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(order.revision, 2);
        let snapshot = service.snapshot().expect("snapshot");
        let fill = snapshot
            .fills
            .iter()
            .find(|fill| fill.order_id == order.id)
            .expect("fill");
        assert_eq!(fill.price, FixedPoint::try_new(expected, 2).expect("price"));
        assert_eq!(fill.provenance.observed_unix_nanos, 1_000_001_000_000);
        let mut events = snapshot
            .order_events
            .iter()
            .filter(|event| event.order_id == order.id)
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.sequence);
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            vec![OrderEventKind::Accepted, OrderEventKind::Filled]
        );
        let before = service.snapshot().expect("before restart");
        service.shutdown(Duration::from_secs(5)).expect("shutdown");
        let restarted = TradingService::start(config(&directory)).expect("restart");
        let after = restarted.snapshot().expect("restored");
        assert_eq!(after.orders, before.orders);
        assert_eq!(after.order_events, before.order_events);
        assert_eq!(after.fills, before.fills);
        assert_eq!(after.positions, before.positions);
        restarted
            .shutdown(Duration::from_secs(5))
            .expect("shutdown");
    }
}

#[test]
fn nonmarketable_ioc_and_fok_cancel_in_the_acceptance_step() {
    let directory = TestDirectory::new("immediate-not-marketable");
    let (service, _clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .expect("feed");
    feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
    feed.push(live_session(route().instrument_id, 1))
        .expect("session");
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .expect("live");
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .expect("quote");
    service.status().expect("drain");
    for (tif, side, price) in [
        (TimeInForce::ImmediateOrCancel, OrderSide::Buy, 9_900),
        (TimeInForce::FillOrKill, OrderSide::Buy, 9_900),
        (TimeInForce::ImmediateOrCancel, OrderSide::Sell, 10_100),
        (TimeInForce::FillOrKill, OrderSide::Sell, 10_100),
    ] {
        let mut request = market_order(
            &format!("unfilled-{tif:?}-{side:?}"),
            side,
            1,
            1_000_000_000_000,
        );
        request.order_type = OrderType::Limit;
        request.time_in_force = tif;
        request.limit_price = Some(FixedPoint::try_new(price, 2).expect("price"));
        let accepted = service.place_order(request).expect("accepted");
        assert_eq!(accepted.status, OrderStatus::Cancelled);
        let snapshot = service.snapshot().expect("snapshot");
        let mut events: Vec<_> = snapshot
            .order_events
            .iter()
            .filter(|event| event.order_id == accepted.id)
            .collect();
        events.sort_by_key(|event| event.sequence);
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            vec![OrderEventKind::Accepted, OrderEventKind::Cancelled]
        );
        assert_eq!(
            events[1].reason,
            Some(aeris_trading::OrderEventReason::NotMarketable)
        );
        assert!(
            snapshot
                .fills
                .iter()
                .all(|fill| fill.order_id != accepted.id)
        );
    }
    let before = service.snapshot().expect("before restart");
    service.shutdown(Duration::from_secs(5)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    assert_eq!(
        restarted.snapshot().expect("restored").orders,
        before.orders
    );
    restarted
        .shutdown(Duration::from_secs(5))
        .expect("shutdown");
}

#[test]
fn market_without_quote_waits_five_seconds_then_cancels_or_fills() {
    let directory = TestDirectory::new("market-no-quote-deadline");
    let (service, clock) = feed_service(&directory);
    service.set_ticket_interest(vec![route()]).expect("ticket");
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .expect("feed");
    feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
    feed.push(live_session(route().instrument_id, 1))
        .expect("session");
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .expect("live");
    service.status().expect("drain");

    let waiting = service
        .place_order(market_order(
            "market-timeout",
            OrderSide::Buy,
            1,
            1_000_000_000_000,
        ))
        .expect("live feed without quote admits market order");
    assert_eq!(waiting.status, OrderStatus::Working);
    clock.advance(Duration::from_millis(4_999));
    assert_eq!(
        service.snapshot().unwrap().orders[0].status,
        OrderStatus::Working
    );
    clock.advance(Duration::from_millis(1));
    let expired = service.snapshot().unwrap();
    let cancelled = expired
        .orders
        .iter()
        .find(|order| order.id == waiting.id)
        .unwrap();
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(cancelled.terminal_unix_nanos, Some(1_005_000_000_000));
    assert_eq!(
        expired
            .order_events
            .iter()
            .find(|event| event.order_id == waiting.id && event.kind == OrderEventKind::Cancelled)
            .expect("timeout event")
            .reason,
        Some(aeris_trading::OrderEventReason::NoQuote)
    );
    assert_eq!(expired.fills.len(), 0);

    let arriving = service
        .place_order(market_order(
            "market-arrives",
            OrderSide::Buy,
            1,
            1_005_000_000_000,
        ))
        .expect("second waiting market order");
    let mut arriving_quote = quote(1, 1, 10_000, 10_025);
    arriving_quote.received_unix_nanos = 1_005_001_000_000;
    feed.push(MarketFeedEvent::Quote(arriving_quote))
        .expect("fresh quote");
    service.status().expect("drain");
    let filled = service.snapshot().unwrap();
    assert_eq!(
        filled
            .orders
            .iter()
            .find(|order| order.id == arriving.id)
            .unwrap()
            .status,
        OrderStatus::Filled
    );
    assert_eq!(filled.fills.len(), 1);
    assert_eq!(
        filled.fills[0].price,
        FixedPoint::try_new(10_025, 2).unwrap()
    );
    assert_eq!(
        filled
            .orders
            .iter()
            .find(|order| order.id == waiting.id)
            .unwrap()
            .status,
        OrderStatus::Cancelled
    );
    service.shutdown(Duration::from_secs(5)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    let restored = restarted.snapshot().expect("restored");
    assert_eq!(restored.orders, filled.orders);
    assert_eq!(restored.order_events, filled.order_events);
    assert_eq!(restored.fills, filled.fills);
    restarted
        .shutdown(Duration::from_secs(5))
        .expect("shutdown");
}

#[test]
fn stops_trigger_at_exact_and_gapped_touches_on_the_correct_side() {
    for (index, side, stop, before_bid, before_ask, gap_bid, gap_ask, expected) in [
        (
            0,
            OrderSide::Buy,
            10_100,
            10_074,
            10_099,
            10_275,
            10_300,
            10_300,
        ),
        (1, OrderSide::Sell, 9_900, 9_901, 9_926, 9_700, 9_725, 9_700),
        (
            2,
            OrderSide::Buy,
            10_100,
            10_074,
            10_099,
            10_075,
            10_100,
            10_100,
        ),
        (3, OrderSide::Sell, 9_900, 9_901, 9_926, 9_900, 9_925, 9_900),
    ] {
        let directory = TestDirectory::new(&format!("stop-gap-{index}"));
        let (service, clock) = feed_service(&directory);
        service.set_ticket_interest(vec![route()]).expect("ticket");
        let feed = service
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .expect("feed");
        feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
        feed.push(live_session(route().instrument_id, 1))
            .expect("session");
        feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
            .expect("live");
        feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
            .expect("initial quote");
        service.status().expect("drain");
        let mut request = market_order(&format!("stop-gap-{index}"), side, 1, 1_000_000_000_000);
        request.order_type = OrderType::Stop;
        request.stop_price = Some(FixedPoint::try_new(stop, 2).unwrap());
        let order = service.place_order(request).expect("working stop");
        clock.advance(Duration::from_millis(2));
        feed.push(MarketFeedEvent::Quote(quote(1, 2, before_bid, before_ask)))
            .expect("near quote");
        service.status().expect("drain");
        assert_eq!(
            service
                .snapshot()
                .unwrap()
                .orders
                .iter()
                .find(|current| current.id == order.id)
                .unwrap()
                .status,
            OrderStatus::Working
        );
        clock.advance(Duration::from_millis(1));
        feed.push(MarketFeedEvent::Quote(quote(1, 3, gap_bid, gap_ask)))
            .expect("gapped quote");
        service.status().expect("drain");
        let snapshot = service.snapshot().expect("snapshot");
        let filled = snapshot
            .orders
            .iter()
            .find(|current| current.id == order.id)
            .unwrap();
        assert_eq!(filled.status, OrderStatus::Filled);
        assert_eq!(filled.triggered_unix_nanos, Some(1_000_003_000_000));
        assert_eq!(filled.revision, 3);
        let fill = snapshot
            .fills
            .iter()
            .find(|fill| fill.order_id == order.id)
            .unwrap();
        assert_eq!(fill.price, FixedPoint::try_new(expected, 2).unwrap());
        let mut events = snapshot
            .order_events
            .iter()
            .filter(|event| event.order_id == order.id)
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.sequence);
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            [
                OrderEventKind::Accepted,
                OrderEventKind::Triggered,
                OrderEventKind::Filled
            ]
        );
        assert_eq!(events[1].event_unix_nanos, 1_000_003_000_000);
        assert_eq!(events[2].event_unix_nanos, 1_000_003_000_000);
        service.shutdown(Duration::from_secs(5)).expect("shutdown");
        assert_stop_restart(&directory, &snapshot);
    }
}

fn assert_stop_restart(directory: &TestDirectory, snapshot: &TradingSnapshot) {
    let restarted = TradingService::start(config(directory)).expect("restart");
    let restored = restarted.snapshot().expect("restored");
    assert_eq!(restored.orders, snapshot.orders);
    assert_eq!(restored.order_events, snapshot.order_events);
    assert_eq!(restored.fills, snapshot.fills);
    restarted
        .shutdown(Duration::from_secs(5))
        .expect("shutdown");
}

#[test]
fn stop_limit_gap_through_triggers_then_rests_at_limit_across_restart() {
    for (index, side, stop, limit, gap_bid, gap_ask, later_bid, later_ask) in [
        (
            0,
            OrderSide::Buy,
            10_100,
            10_150,
            10_275,
            10_300,
            10_115,
            10_140,
        ),
        (1, OrderSide::Sell, 9_900, 9_850, 9_700, 9_725, 9_860, 9_885),
    ] {
        let directory = TestDirectory::new(&format!("stop-limit-gap-{index}"));
        let (service, clock) = feed_service(&directory);
        service.set_ticket_interest(vec![route()]).expect("ticket");
        let feed = service
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .expect("feed");
        feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
        feed.push(live_session(route().instrument_id, 1))
            .expect("session");
        feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
            .expect("live");
        feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
            .expect("initial quote");
        service.status().expect("drain");
        let mut request = market_order(&format!("gap-{index}"), side, 1, 1_000_000_000_000);
        request.order_type = OrderType::StopLimit;
        request.stop_price = Some(FixedPoint::try_new(stop, 2).unwrap());
        request.limit_price = Some(FixedPoint::try_new(limit, 2).unwrap());
        let placed = service.place_order(request).expect("working");
        clock.advance(Duration::from_millis(2));
        feed.push(MarketFeedEvent::Quote(quote(1, 2, gap_bid, gap_ask)))
            .expect("gap");
        service.status().expect("drain");
        let at_gap = service.snapshot().expect("gap state");
        let triggered = at_gap
            .orders
            .iter()
            .find(|order| order.id == placed.id)
            .unwrap();
        assert_eq!(triggered.status, OrderStatus::Working);
        assert_eq!(triggered.triggered_unix_nanos, Some(1_000_002_000_000));
        assert_eq!(triggered.revision, 2);
        assert_eq!(at_gap.fills.len(), 0);
        service.shutdown(Duration::from_secs(5)).expect("shutdown");
        let mut config = config(&directory);
        config.clock = clock.clone();
        let restarted = start_service_with_config(config);
        assert_eq!(restarted.snapshot().unwrap().orders, at_gap.orders);
        let feed = restarted
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .expect("feed");
        restarted
            .set_ticket_interest(vec![route()])
            .expect("ticket");
        feed.push(live_session(route().instrument_id, 1))
            .expect("session");
        feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
            .expect("live");
        clock.advance(Duration::from_millis(1));
        feed.push(MarketFeedEvent::Quote(quote(1, 3, later_bid, later_ask)))
            .expect("later quote");
        restarted.status().expect("drain");
        let after = restarted.snapshot().expect("filled");
        assert_eq!(
            after
                .orders
                .iter()
                .find(|order| order.id == placed.id)
                .unwrap()
                .status,
            OrderStatus::Filled
        );
        assert_eq!(after.fills.len(), 1);
        assert_eq!(after.fills[0].price, FixedPoint::try_new(limit, 2).unwrap());
        assert_eq!(
            after
                .order_events
                .iter()
                .filter(
                    |event| event.order_id == placed.id && event.kind == OrderEventKind::Triggered
                )
                .count(),
            1
        );
        restarted
            .shutdown(Duration::from_secs(5))
            .expect("shutdown");
    }
}

#[test]
fn stop_limit_trigger_and_fill_on_one_quote_uses_taker_touch() {
    for (index, side, stop, limit, bid, ask, price) in [
        (0, OrderSide::Buy, 10_100, 10_150, 10_095, 10_120, 10_120),
        (1, OrderSide::Sell, 9_900, 9_850, 9_880, 9_905, 9_880),
    ] {
        let directory = TestDirectory::new(&format!("stop-limit-same-{index}"));
        let (service, clock) = feed_service(&directory);
        let feed = service
            .attach_market_feed(Arc::new(RecordedInterest::default()))
            .expect("feed");
        feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
        feed.push(live_session(route().instrument_id, 1))
            .expect("session");
        feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
            .expect("live");
        feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
            .expect("initial");
        service.status().expect("drain");
        let mut request = market_order(&format!("same-{index}"), side, 1, 1_000_000_000_000);
        request.order_type = OrderType::StopLimit;
        request.stop_price = Some(FixedPoint::try_new(stop, 2).unwrap());
        request.limit_price = Some(FixedPoint::try_new(limit, 2).unwrap());
        let placed = service.place_order(request).expect("working");
        clock.advance(Duration::from_millis(2));
        feed.push(MarketFeedEvent::Quote(quote(1, 2, bid, ask)))
            .expect("triggering quote");
        service.status().expect("drain");
        let snapshot = service.snapshot().expect("snapshot");
        assert_eq!(
            snapshot
                .orders
                .iter()
                .find(|order| order.id == placed.id)
                .unwrap()
                .status,
            OrderStatus::Filled
        );
        assert_eq!(
            snapshot.fills[0].price,
            FixedPoint::try_new(price, 2).unwrap()
        );
        let mut events = snapshot
            .order_events
            .iter()
            .filter(|event| event.order_id == placed.id)
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.sequence);
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            [
                OrderEventKind::Accepted,
                OrderEventKind::Triggered,
                OrderEventKind::Filled
            ]
        );
        assert_eq!(events[1].event_unix_nanos, events[2].event_unix_nanos);
        service.shutdown(Duration::from_secs(5)).expect("shutdown");
    }
}

#[test]
fn waiting_market_deadline_survives_restart_without_a_feed() {
    let directory = TestDirectory::new("market-wait-restart");
    let (service, clock) = feed_service(&directory);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .expect("feed");
    feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
    feed.push(live_session(route().instrument_id, 1))
        .expect("session");
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .expect("live");
    service.status().expect("drain");
    let waiting = service
        .place_order(market_order(
            "restart-wait",
            OrderSide::Buy,
            1,
            1_000_000_000_000,
        ))
        .expect("waiting");
    service.shutdown(Duration::from_secs(5)).expect("shutdown");
    clock.advance(Duration::from_secs(5));
    let mut configuration = config(&directory);
    configuration.clock = clock;
    let restarted = start_service_with_config(configuration);
    let snapshot = restarted.snapshot().expect("expired on recovery");
    let order = snapshot
        .orders
        .iter()
        .find(|order| order.id == waiting.id)
        .unwrap();
    assert_eq!(order.status, OrderStatus::Cancelled);
    assert_eq!(order.revision, 2);
    assert_eq!(order.terminal_unix_nanos, Some(1_005_000_000_000));
    assert_eq!(snapshot.fills.len(), 0);
    restarted
        .shutdown(Duration::from_secs(5))
        .expect("shutdown");
}

#[test]
fn resting_at_uses_owner_quote_and_persists_only_resolved_order() {
    let directory = TestDirectory::new("resting-at-owner");
    let (service, _clock) = feed_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut missing_quote = market_order("resting-no-quote", OrderSide::Buy, 1, 1_000);
    missing_quote.order_type = OrderType::RestingAt;
    missing_quote.limit_price = Some(FixedPoint::try_new(10_050, 2).expect("price"));
    let before = service.snapshot().expect("before");
    assert_eq!(
        service
            .place_order(missing_quote)
            .expect_err("no owner quote"),
        aeris_trading::RejectReason::QuoteUnavailable.to_string()
    );
    assert_eq!(service.snapshot().expect("unchanged"), before);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .expect("feed");
    feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
    feed.push(live_session(route().instrument_id, 1))
        .expect("session");
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .expect("live");
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .expect("quote");
    service.status().expect("drain");
    for (id, side, units, expected) in [
        ("resting-buy-limit", OrderSide::Buy, 9_900, OrderType::Limit),
        ("resting-buy-stop", OrderSide::Buy, 10_050, OrderType::Stop),
        (
            "resting-sell-limit",
            OrderSide::Sell,
            10_100,
            OrderType::Limit,
        ),
        ("resting-sell-stop", OrderSide::Sell, 9_975, OrderType::Stop),
    ] {
        let mut request = market_order(id, side, 1, 1_000);
        request.order_type = OrderType::RestingAt;
        request.time_in_force = TimeInForce::GoodTillCancelled;
        request.limit_price = Some(FixedPoint::try_new(units, 2).expect("price"));
        let placed = service.place_order(request).expect("placed");
        assert_eq!(placed.order_type, expected);
        assert_eq!(
            placed.limit_price,
            (expected == OrderType::Limit).then_some(FixedPoint::try_new(units, 2).unwrap())
        );
        assert_eq!(
            placed.stop_price,
            (expected == OrderType::Stop).then_some(FixedPoint::try_new(units, 2).unwrap())
        );
    }
    let before = service.snapshot().expect("snapshot");
    service.shutdown(Duration::from_secs(5)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    assert_eq!(
        restarted.snapshot().expect("restored").orders,
        before.orders
    );
    restarted
        .shutdown(Duration::from_secs(5))
        .expect("shutdown");
}

#[test]
fn wrong_side_stops_share_owner_quote_refusal_across_both_routes() {
    let directory = TestDirectory::new("stop-route-parity");
    let (service, _clock) = feed_service(&directory);
    let broker_id = TradingAccountId::try_new("stop-parity-demo").expect("account id");
    service
        .register_account(TradingAccount {
            id: broker_id.clone(),
            display_name: "Scripted demo".into(),
            environment: AccountEnvironment::Demo,
            venue_id: "ctrader".into(),
            broker_ref: Some("fixture-demo".into()),
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("demo account");
    let (_generation, outbound) = service.attach_demo_venue().expect("venue");
    while outbound.try_recv().is_ok() {}
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .expect("feed");
    feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
    feed.push(live_session(route().instrument_id, 1))
        .expect("session");
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .expect("live");
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .expect("quote");
    service.status().expect("drain");
    let before = service.snapshot().expect("before");
    for (side, stop_units) in [(OrderSide::Buy, 10_025), (OrderSide::Sell, 10_000)] {
        for kind in [OrderType::Stop, OrderType::StopLimit] {
            for (route, account_id) in [
                (
                    "simulated",
                    TradingAccountId::try_new("aeris-sim-1").expect("practice id"),
                ),
                ("broker", broker_id.clone()),
            ] {
                let mut command = market_order(
                    &format!("wrong-side-{side:?}-{kind:?}-{route}"),
                    side,
                    1,
                    1_000_000_000_000,
                );
                command.account_id = account_id;
                command.order_type = kind;
                command.time_in_force = TimeInForce::GoodTillCancelled;
                command.stop_price = Some(FixedPoint::try_new(stop_units, 2).expect("stop"));
                if kind == OrderType::StopLimit {
                    command.limit_price = Some(FixedPoint::try_new(10_050, 2).expect("limit"));
                }
                assert_eq!(
                    service.place_order(command).expect_err("wrong-side stop"),
                    aeris_trading::RejectReason::WrongSideTrigger.to_string(),
                    "{side:?}/{kind:?}/{route}"
                );
                assert_eq!(service.snapshot().expect("unchanged"), before);
                assert!(outbound.try_recv().is_err(), "no broker request");
            }
        }
    }
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn chart_modify_checks_revision_before_reading_quote_or_writing_facts() {
    let directory = TestDirectory::new("chart-stale-revision");
    let (service, _clock) = feed_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    let mut request = market_order("chart-stale", OrderSide::Buy, 1, 1_000);
    request.order_type = OrderType::Limit;
    request.time_in_force = TimeInForce::GoodTillCancelled;
    request.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    let accepted = service.place_order(request).expect("accepted");
    let before = service.snapshot().expect("before");
    let command = RevisionedModifyOrder {
        order: ModifyOrder {
            client_order_id: accepted.client_order_id.clone(),
            time_in_force: accepted.time_in_force,
            limit_price: Some(FixedPoint::try_new(9_025, 2).expect("price")),
            stop_price: None,
            modified_unix_nanos: 2_000,
            provenance: accepted.provenance.clone(),
        },
        expected_revision: Some(accepted.revision + 1),
    };
    let result = service.modify_order_from_market_checked(command.clone());
    assert_eq!(
        result.expect_err("stale despite unavailable quote"),
        OrderCommandError::StaleRevision {
            current: Box::new(accepted.clone())
        }
    );
    assert_eq!(service.snapshot().expect("unchanged"), before);
    let feed = service
        .attach_market_feed(Arc::new(RecordedInterest::default()))
        .expect("feed");
    feed.push(MarketFeedEvent::Terms(terms())).expect("terms");
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .expect("live");
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .expect("quote");
    feed.push(live_session(route().instrument_id, 1))
        .expect("session");
    service.status().expect("drain feed");
    let updated = service
        .modify_order_from_market_checked(RevisionedModifyOrder {
            expected_revision: Some(accepted.revision),
            ..command
        })
        .expect("current chart revision");
    assert_eq!(updated.revision, accepted.revision + 1);
    assert_eq!(updated.limit_price.expect("limit").units(), 9_025);
    assert_eq!(updated.provenance.provider_id, "fixture");
    assert!(
        service
            .snapshot()
            .expect("modified")
            .orders
            .contains(&updated)
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = start_service(&directory);
    assert!(
        restarted
            .snapshot()
            .expect("recovered")
            .orders
            .contains(&updated)
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
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
    let PlacementOutcome::Managed(bracket) = placed else {
        panic!("expected managed bracket");
    };
    assert_eq!(bracket.template.stop_offset_ticks, 3);
    assert_eq!(bracket.template.targets[0].offset_ticks, 4);
    // The resting entry plus its held stop and target legs are real orders at placement.
    assert_eq!(service.snapshot().unwrap().orders.len(), 3);
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
            // Four plain orders plus the bracket's entry and its held legs; the one-lot
            // entry rounds the 50% first target out, leaving a stop and one target.
            assert_eq!(after.orders.len(), 7);
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

#[test]
fn interest_revision_survives_owner_restart() {
    let directory = TestDirectory::new("interest-revision-restart");
    let configuration = config(&directory);
    let open = || {
        let (commands, _receiver) = mpsc::sync_channel(1);
        let feed = Arc::new(crate::core::market::feed::MarketFeedInbox::new(
            commands,
            Arc::new(AtomicBool::new(false)),
        ));
        crate::TradingCore::open(
            &configuration.database_path,
            configuration.retention,
            configuration.clock.clone(),
            configuration.execution,
            std::sync::Weak::new(),
            feed,
        )
        .unwrap()
    };
    let publish = |owner: &mut crate::TradingCore, sink: &Arc<RecordedInterest>| {
        let (reply, response) = mpsc::sync_channel(1);
        owner.dispatch_market_command(Command::AttachMarketFeed(sink.clone(), reply));
        response.recv().unwrap().unwrap();
        owner.replace_ticket_interest(vec![route()]).unwrap();
        owner.recompute_quote_interest().unwrap();
        sink.0
            .lock()
            .unwrap()
            .iter()
            .map(|interest| interest.revision)
            .collect::<Vec<_>>()
    };
    let first = publish(&mut open(), &Arc::new(RecordedInterest::default()));
    assert_eq!(first, vec![1]);
    let second = publish(&mut open(), &Arc::new(RecordedInterest::default()));
    assert_eq!(second, vec![2]);
}

#[derive(Default)]
pub(crate) struct RecordedInterest(Mutex<Vec<QuoteInterest>>);

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

fn seeded_quote_book() -> (
    crate::core::market::feed::QuoteBook,
    BTreeMap<InstrumentId, MarketRoute>,
) {
    let mut book = crate::core::market::feed::QuoteBook::default();
    let routes = [(route().instrument_id.clone(), route())].into();
    assert!(
        book.accept(MarketFeedEvent::Quote(quote(2, 4, 10_000, 10_025)), &routes)
            .unwrap()
    );
    (book, routes)
}

#[test]
fn quote_book_rejects_non_positive_prices_without_retiring_quote() {
    let (mut book, routes) = seeded_quote_book();
    for (bid, ask) in [(0, 10_025), (-1, 10_025), (10_000, 0), (10_000, -1)] {
        assert_eq!(
            book.accept(MarketFeedEvent::Quote(quote(3, 5, bid, ask)), &routes),
            Err(MarketFeedError::InvalidSpread)
        );
        assert_eq!(
            book.quote(&route().instrument_id).unwrap().source_sequence,
            Some(4)
        );
    }
}

#[test]
fn quote_book_rejects_crossed_or_equal_spread_without_retiring_quote() {
    let (mut book, routes) = seeded_quote_book();
    for (bid, ask) in [(10_025, 10_025), (10_026, 10_025)] {
        assert_eq!(
            book.accept(MarketFeedEvent::Quote(quote(3, 5, bid, ask)), &routes),
            Err(MarketFeedError::InvalidSpread)
        );
        assert_eq!(
            book.quote(&route().instrument_id).unwrap().source_sequence,
            Some(4)
        );
    }
}

#[test]
fn quote_book_rejects_wrong_bid_or_ask_scale_without_retiring_quote() {
    let (mut book, routes) = seeded_quote_book();
    for bid_side in [true, false] {
        let mut invalid = quote(3, 5, 10_000, 10_025);
        if bid_side {
            invalid.bid = FixedPoint::try_new(10_000, 3).unwrap();
        } else {
            invalid.ask = FixedPoint::try_new(10_025, 3).unwrap();
        }
        assert_eq!(
            book.accept(MarketFeedEvent::Quote(invalid), &routes),
            Err(MarketFeedError::InvalidScale)
        );
        assert_eq!(
            book.quote(&route().instrument_id).unwrap().source_sequence,
            Some(4)
        );
    }
}

#[test]
fn quote_book_ignores_older_provider_generation() {
    let (mut book, routes) = seeded_quote_book();
    assert!(
        !book
            .accept(MarketFeedEvent::Quote(quote(1, 99, 9_900, 10_000)), &routes)
            .unwrap()
    );
    assert_eq!(
        book.quote(&route().instrument_id).unwrap().source_sequence,
        Some(4)
    );
}

#[test]
fn quote_book_ignores_lower_source_sequence_in_same_generation() {
    let (mut book, routes) = seeded_quote_book();
    assert!(
        !book
            .accept(MarketFeedEvent::Quote(quote(2, 3, 9_900, 10_000)), &routes)
            .unwrap()
    );
    assert_eq!(
        book.quote(&route().instrument_id).unwrap().source_sequence,
        Some(4)
    );
}

#[test]
fn mixed_control_overflow_drops_oldest_terms_and_requests_full_refresh() {
    let (commands, receiver) = mpsc::sync_channel(1);
    let inbox =
        crate::core::market::feed::MarketFeedInbox::new(commands, Arc::new(AtomicBool::new(false)));
    let mut oldest = terms();
    oldest.route.provider_symbol = "OLDEST".into();
    inbox.push(MarketFeedEvent::Terms(oldest)).unwrap();
    for index in 0..1_023 {
        let event = if index % 2 == 0 {
            feed_state(route().instrument_id, 1, FeedState::Live)
        } else {
            live_session(route().instrument_id, 1)
        };
        inbox.push(event).unwrap();
    }
    assert!(matches!(receiver.try_recv(), Ok(Command::DrainMarketFeed)));
    assert!(
        receiver.try_recv().is_err(),
        "only one drain wake is queued"
    );
    inbox
        .push(MarketFeedEvent::Quote(quote(2, 3, 10_000, 10_025)))
        .unwrap();
    inbox
        .push(MarketFeedEvent::Quote(quote(2, 2, 9_900, 10_000)))
        .unwrap();
    inbox
        .push(MarketFeedEvent::Quote(quote(2, 4, 10_050, 10_075)))
        .unwrap();
    inbox.push(MarketFeedEvent::Terms(terms())).unwrap();
    assert!(receiver.try_recv().is_err());
    let batch = inbox.drain();
    assert_eq!(batch.quotes.len(), 1);
    assert_eq!(batch.quotes[0].source_sequence, Some(4));
    assert_eq!(batch.controls.len(), 1_024);
    assert!(matches!(&batch.controls[0], MarketFeedEvent::Feed { .. }));
    assert!(matches!(&batch.controls[1], MarketFeedEvent::Session(_)));
    assert!(
        matches!(&batch.controls[1_023], MarketFeedEvent::Terms(terms) if terms.route.provider_symbol == "ESZ6")
    );
    assert!(batch.resync);

    let directory = TestDirectory::new("mixed-overflow-refresh");
    let configuration = config(&directory);
    let (commands, _receiver) = mpsc::sync_channel(1);
    let feed = Arc::new(crate::core::market::feed::MarketFeedInbox::new(
        commands,
        Arc::new(AtomicBool::new(false)),
    ));
    let mut owner = crate::TradingCore::open(
        &configuration.database_path,
        configuration.retention,
        configuration.clock,
        configuration.execution,
        std::sync::Weak::new(),
        feed.clone(),
    )
    .unwrap();
    let sink = Arc::new(RecordedInterest::default());
    let (reply, response) = mpsc::sync_channel(1);
    owner.dispatch_market_command(Command::AttachMarketFeed(sink.clone(), reply));
    response.recv().unwrap().unwrap();
    owner.replace_ticket_interest(vec![route()]).unwrap();
    owner.recompute_quote_interest().unwrap();
    let published = sink.0.lock().unwrap().clone();
    assert_eq!(published.len(), 1);
    for index in 0..1_025 {
        let mut control = terms();
        control.route.provider_symbol = format!("OVERFLOW-{index}");
        feed.push(MarketFeedEvent::Terms(control)).unwrap();
    }
    owner.drain_market_feed();
    owner.recompute_quote_interest().unwrap();
    let updates = sink.0.lock().unwrap();
    assert_eq!(
        updates.len(),
        2,
        "resync republishes a full interest snapshot"
    );
    assert_eq!(updates[1].routes, published[0].routes);
    assert!(updates[1].revision > published[0].revision);
    drop(updates);
}

#[test]
fn market_feed_push_does_not_wait_for_undrained_owner() {
    let (commands, receiver) = mpsc::sync_channel(1);
    commands.try_send(Command::DrainMarketFeed).unwrap();
    let inbox = Arc::new(crate::core::market::feed::MarketFeedInbox::new(
        commands,
        Arc::new(AtomicBool::new(false)),
    ));
    let (done, finished) = mpsc::sync_channel(1);
    let sender = std::thread::spawn(move || {
        for index in 0..2_048 {
            let event = if index % 2 == 0 {
                MarketFeedEvent::Terms(terms())
            } else {
                MarketFeedEvent::Quote(quote(2, 4, 10_000, 10_025))
            };
            inbox.push(event).unwrap();
        }
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(Duration::from_secs(2))
        .expect("push must finish while the owner does not drain");
    sender.join().unwrap();
    assert!(matches!(receiver.try_recv(), Ok(Command::DrainMarketFeed)));
    assert!(receiver.try_recv().is_err());
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
fn quote_interest_tracks_orders_held_legs_positions_tickets_and_capacity() {
    let directory = TestDirectory::new("complete-interest-lifecycle");
    let (service, _clock) = feed_service(&directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink.clone()).unwrap();
    let published = |mut expected: Vec<MarketRoute>| {
        service.status().unwrap();
        expected.sort_by(|left, right| left.instrument_id.cmp(&right.instrument_id));
        assert_eq!(
            sink.0
                .lock()
                .unwrap()
                .last()
                .map(|item| item.routes.clone())
                .unwrap_or_default(),
            expected
        );
    };
    let count = || sink.0.lock().unwrap().len();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    published(vec![]);

    let mut limit = market_order("interest-limit", OrderSide::Buy, 1, 1_000);
    limit.order_type = OrderType::Limit;
    limit.time_in_force = TimeInForce::GoodTillCancelled;
    limit.limit_price = Some(FixedPoint::try_new(9_000, 2).unwrap());
    service.place_order(limit).unwrap();
    published(vec![route()]);
    let sends = count();
    service.set_ticket_interest(vec![route()]).unwrap();
    published(vec![route()]);
    assert_eq!(count(), sends, "an unchanged union sends nothing");
    service.set_ticket_interest(vec![]).unwrap();
    published(vec![route()]);
    assert_eq!(count(), sends);
    service
        .cancel_order(ClientOrderId::try_new("interest-limit").unwrap())
        .unwrap();
    published(vec![]);

    service.set_ticket_interest(vec![route()]).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 2, 10_000, 10_025)))
        .unwrap();
    published(vec![route()]);
    let mut entry = market_order("interest-bracket", OrderSide::Buy, 2, 1_000);
    entry.order_type = OrderType::Limit;
    entry.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    let bracket = service
        .place_chart_bracket_from_market(PlaceChartBracket {
            entry,
            stop_price: FixedPoint::try_new(9_800, 2).unwrap(),
            target_price: FixedPoint::try_new(10_000, 2).unwrap(),
        })
        .unwrap();
    assert!(matches!(bracket, PlacementOutcome::Managed(_)));
    let snapshot = service.snapshot().unwrap();
    assert_eq!(
        snapshot
            .orders
            .iter()
            .filter(|order| order.status.is_open())
            .count(),
        3,
        "the entry and its held stop and target legs are all open orders"
    );
    assert_eq!(
        snapshot
            .orders
            .iter()
            .filter(|order| order.status == OrderStatus::Inactive)
            .count(),
        2,
        "the bracket legs wait inactive for the entry fill"
    );
    assert_eq!(
        snapshot.managed_brackets[0].status,
        BracketViewStatus::AwaitingEntry
    );
    service.set_ticket_interest(Vec::new()).unwrap();
    // The resting entry and its inactive legs keep the instrument's quote lease.
    published(vec![route()]);
    service
        .cancel_order(ClientOrderId::try_new("interest-bracket").unwrap())
        .unwrap();
    assert_eq!(
        service.snapshot().unwrap().managed_brackets[0].status,
        BracketViewStatus::Cancelled
    );
    published(vec![]);

    assert_position_interest(&service, &feed, &published, &count);
    assert_ticket_interest(&service, &published);
    service.shutdown(Duration::from_secs(2)).unwrap();
    assert_group_only_interest(&directory);
    assert_sink_interest_capacity();
}

fn assert_position_interest(
    service: &TradingService,
    feed: &Arc<crate::core::market::feed::MarketFeedInbox>,
    published: &impl Fn(Vec<MarketRoute>),
    count: &impl Fn() -> usize,
) {
    service.set_ticket_interest(vec![route()]).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 3, 10_000, 10_025)))
        .unwrap();
    published(vec![route()]);
    service
        .place_order_from_market(
            market_order("interest-long", OrderSide::Buy, 3, 1_000),
            false,
        )
        .unwrap();
    service.set_ticket_interest(Vec::new()).unwrap();
    published(vec![route()]); // The filled order is terminal; the position owns the lease.
    let sends = count();
    feed.push(MarketFeedEvent::Quote(quote(1, 4, 10_100, 10_125)))
        .unwrap();
    published(vec![route()]);
    assert_eq!(sends, count(), "a mark-only change does not resend demand");
    service
        .place_order_from_market(
            market_order("interest-flat", OrderSide::Sell, 5, 1_000),
            false,
        )
        .unwrap();
    published(vec![]);
    feed.push(MarketFeedEvent::Quote(quote(1, 6, 10_100, 10_125)))
        .unwrap();
    published(vec![]);
    assert_eq!(
        service.snapshot().unwrap().positions[0]
            .net_quantity
            .units(),
        0
    );
}

fn assert_ticket_interest(service: &TradingService, published: &impl Fn(Vec<MarketRoute>)) {
    let tickets = (0..64)
        .map(|index| {
            let mut ticket = route();
            ticket.instrument_id =
                InstrumentId::try_new(format!("instrument:fixture:ticket:{index}")).unwrap();
            ticket
        })
        .collect::<Vec<_>>();
    service.set_ticket_interest(tickets.clone()).unwrap();
    published(tickets.clone());
    assert_eq!(
        service.set_ticket_interest(vec![route(); 65]).unwrap_err(),
        "ticket quote interest limit reached"
    );
    published(tickets);
    service.set_ticket_interest(Vec::new()).unwrap();
    published(vec![]);
}

fn assert_group_only_interest(directory: &TestDirectory) {
    // A real pending group is the only interest source on its instrument: placing it
    // publishes the route and cancelling its entry releases the lease through the cascade.
    let (service, _clock) = feed_service(directory);
    let sink = Arc::new(RecordedInterest::default());
    let feed = service.attach_market_feed(sink.clone()).unwrap();
    feed.push(MarketFeedEvent::Terms(terms())).unwrap();
    feed.push(live_session(route().instrument_id, 1)).unwrap();
    feed.push(feed_state(route().instrument_id, 1, FeedState::Live))
        .unwrap();
    feed.push(MarketFeedEvent::Quote(quote(1, 1, 10_000, 10_025)))
        .unwrap();
    service.status().unwrap();
    let mut entry = market_order("group-only-interest", OrderSide::Buy, 9, 1_000);
    entry.order_type = OrderType::Limit;
    entry.time_in_force = TimeInForce::GoodTillCancelled;
    entry.limit_price = Some(FixedPoint::try_new(9_900, 2).unwrap());
    entry.quantity = FixedPoint::try_new(2, 0).unwrap();
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: super::fixed_stop_bracket_template(),
        })
        .unwrap();
    service.status().unwrap();
    let snapshot = service.snapshot().unwrap();
    assert_eq!(
        snapshot
            .orders
            .iter()
            .filter(|order| order.status == OrderStatus::Inactive)
            .count(),
        3,
        "the held stop and both targets are inactive orders"
    );
    let sent = || {
        sink.0
            .lock()
            .unwrap()
            .last()
            .map(|item| item.routes.clone())
            .unwrap_or_default()
    };
    assert_eq!(sent(), vec![route()], "the group alone holds the lease");
    service
        .cancel_order(ClientOrderId::try_new("group-only-interest").unwrap())
        .unwrap();
    service.status().unwrap();
    assert_eq!(sent(), Vec::<MarketRoute>::new(), "the cascade releases it");
    service.shutdown(Duration::from_secs(2)).unwrap();
}

fn assert_sink_interest_capacity() {
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
            let before = service.snapshot().unwrap();
            let previous_sends = sink.0.lock().unwrap().len();
            assert_eq!(
                service.place_order(order).unwrap_err(),
                "CapacityExceeded: trading order capacity reached"
            );
            assert_eq!(
                service.snapshot().unwrap(),
                before,
                "257th order has no facts"
            );
            assert_eq!(sink.0.lock().unwrap().len(), previous_sends);
        } else {
            service.place_order(order).unwrap();
        }
        service.status().unwrap();
        let history = sink.0.lock().unwrap();
        assert_eq!(
            history.last().unwrap().routes,
            (0..=index.min(255))
                .map(|number| {
                    let mut expected = route();
                    expected.instrument_id =
                        InstrumentId::try_new(format!("instrument:fixture:{number}")).unwrap();
                    (expected.instrument_id.clone(), expected)
                })
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_values()
                .collect::<Vec<_>>()
        );
    }
    let history = sink.0.lock().unwrap();
    assert_eq!(history.last().unwrap().routes.len(), 256);
    assert!(history.iter().all(|interest| interest.routes.len() <= 256));
    drop(history);
    assert_eq!(service.snapshot().unwrap().orders.len(), 256);
    service.shutdown(Duration::from_secs(2)).unwrap();
}
