use super::*;

fn money(units: i64) -> FixedPoint {
    FixedPoint::try_new(units, 2).expect("money")
}

fn account() -> TradingAccountId {
    TradingAccountId::try_new("aeris-sim-1").expect("account")
}

/// A practice profile whose daily loss and session rules stay out of the way of the
/// prop-firm rule under test.
fn prop_firm_profile(
    version: u32,
    daily_profit_target: Option<FixedPoint>,
    maximum_drawdown: Option<MaximumDrawdownRule>,
) -> RiskProfile {
    RiskProfile {
        account_id: account(),
        profile_id: "prop-firm".to_string(),
        version,
        session_start_unix_nanos: rollover_wall_nanos("2026-10-16T12:00:00Z"),
        session_start_realized_pnl: money(0),
        daily_loss_limit: money(100_000),
        trailing_drawdown: None,
        trailing_mode: TrailingDrawdownMode::Intraday,
        max_contracts: FixedPoint::try_new(5, 0).expect("contracts"),
        consistency_max_single_trade_percent: None,
        restricted_until_unix_nanos: None,
        economic_event_rule: None,
        daily_profit_target,
        maximum_drawdown,
        enabled: true,
    }
}

/// A practice account at $50,000.00 on a deterministic clock at 2026-10-16T12:00Z with the
/// futures fixture's verified terms and a session open across the next week's boundaries.
fn prop_firm_world(
    directory: &TestDirectory,
) -> (
    TradingService,
    Arc<crate::clock::ManualClock>,
    Arc<crate::core::market::feed::MarketFeedInbox>,
) {
    let (service, clock, feed) = live_feed_service(directory);
    clock.set_wall_unix_nanos(rollover_wall_nanos("2026-10-16T12:00:00Z"));
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    open_session_feed(&feed, &instrument());
    service.status().expect("feed drains");
    (service, clock, feed)
}

/// Moves the clock to `wall`, delivers one Live quote received then and waits for the owner.
fn quote_at(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
    clock: &Arc<crate::clock::ManualClock>,
    wall: &str,
    sequence: u64,
    bid: i64,
) {
    clock.set_wall_unix_nanos(rollover_wall_nanos(wall));
    push_quote_at_clock(feed, &instrument(), clock, sequence, bid, bid + 25);
    service.status().expect("quote drains");
}

/// Buys one contract at $100.00 ($50 a point) through the owner's next quote.
fn buy_one_at_one_hundred(
    service: &TradingService,
    feed: &crate::core::market::feed::MarketFeedInbox,
    clock: &Arc<crate::clock::ManualClock>,
) {
    service
        .place_order_caller_priced(market_order(
            "prop-entry",
            OrderSide::Buy,
            1,
            rollover_wall_nanos("2026-10-16T12:00:01Z"),
        ))
        .expect("entry accepted");
    quote_at(service, feed, clock, "2026-10-16T12:00:02Z", 2, 9_975);
    assert_eq!(
        service.snapshot().expect("entry snapshot").fills.len(),
        1,
        "the entry fills at the $100.00 ask"
    );
}

fn new_exposure(service: &TradingService, client_id: &str, wall: &str) -> Result<(), String> {
    service
        .place_order_caller_priced(market_order(
            client_id,
            OrderSide::Buy,
            9,
            rollover_wall_nanos(wall),
        ))
        .map(|_| ())
}

fn lock_kinds(service: &TradingService) -> Vec<RiskLockKind> {
    service
        .snapshot()
        .expect("lock snapshot")
        .risk_locks
        .iter()
        .map(|lock| lock.kind)
        .collect()
}

fn meter(service: &TradingService) -> RiskMeter {
    let mut meters = service.snapshot().expect("meter snapshot").risk_meters;
    assert_eq!(meters.len(), 1);
    meters.remove(0)
}

fn equity_peak(service: &TradingService) -> Option<FixedPoint> {
    service.snapshot().expect("rule snapshot").risk_rule_states[0].equity_peak
}

#[test]
fn the_daily_profit_target_locks_live_and_refuses_new_exposure() {
    let directory = TestDirectory::new("profit-target-live");
    let (service, clock, feed) = prop_firm_world(&directory);
    service
        .register_risk_profile(prop_firm_profile(1, Some(money(10_000)), None))
        .expect("profile registers");
    assert_eq!(meter(&service).profit_target_remaining, Some(money(10_000)));
    buy_one_at_one_hundred(&service, &feed, &clock);

    quote_at(&service, &feed, &clock, "2026-10-16T12:00:03Z", 3, 10_150);
    assert!(
        lock_kinds(&service).is_empty(),
        "+$75.00 is short of the target"
    );
    assert_eq!(meter(&service).profit_target_remaining, Some(money(2_500)));

    quote_at(&service, &feed, &clock, "2026-10-16T12:00:04Z", 4, 10_250);
    assert_eq!(lock_kinds(&service), [RiskLockKind::ProfitTarget]);
    let locked = service.snapshot().expect("locked snapshot");
    assert_eq!(locked.risk_locks[0].reason, "daily profit target reached");
    assert_eq!(
        locked.risk_meters[0].profit_target_remaining,
        Some(money(0))
    );
    let refused = new_exposure(&service, "after-target", "2026-10-16T12:00:05Z")
        .expect_err("the profit target refuses new exposure");
    assert!(
        refused.starts_with("AccountLocked") && refused.ends_with("daily profit target reached"),
        "{refused}"
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("service stops");
}

#[test]
fn the_daily_profit_target_locks_at_admission_and_releases_at_the_session_boundary() {
    let directory = TestDirectory::new("profit-target-admission");
    let (service, clock, feed) = prop_firm_world(&directory);
    buy_one_at_one_hundred(&service, &feed, &clock);
    quote_at(&service, &feed, &clock, "2026-10-16T12:00:03Z", 3, 10_250);
    service
        .place_order_caller_priced(market_order(
            "prop-exit",
            OrderSide::Sell,
            3,
            rollover_wall_nanos("2026-10-16T12:00:04Z"),
        ))
        .expect("exit accepted");
    service.status().expect("exit fills");
    // The session already realized $125.00 when the profile registers, so the first
    // admission against it observes the target.
    service
        .register_risk_profile(prop_firm_profile(1, Some(money(10_000)), None))
        .expect("profile registers");
    assert_eq!(lock_kinds(&service), Vec::<RiskLockKind>::new());
    // A flat account drops its quote interest, so the market runtime republishes the session.
    open_session_feed(&feed, &instrument());
    service.status().expect("session drains");
    let refused = new_exposure(&service, "at-target", "2026-10-16T12:00:05Z")
        .expect_err("admission observes the reached target");
    assert!(refused.contains("daily profit target reached"), "{refused}");
    assert_eq!(lock_kinds(&service), [RiskLockKind::ProfitTarget]);

    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-10-16T23:59:59Z"),
    );
    assert_eq!(
        lock_kinds(&service),
        [RiskLockKind::ProfitTarget],
        "the lock stands for the rest of the session"
    );
    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-10-17T00:00:01Z"),
    );
    assert!(
        lock_kinds(&service).is_empty(),
        "the session boundary releases the profit-target lock"
    );
    assert_eq!(
        meter(&service).profit_target_remaining,
        Some(money(10_000)),
        "the new session measures its own profit"
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("service stops");
}

#[test]
fn an_intraday_maximum_drawdown_peak_persists_and_its_lock_outlasts_the_session() {
    let directory = TestDirectory::new("maximum-drawdown-intraday");
    let (service, clock, feed) = prop_firm_world(&directory);
    let rule = MaximumDrawdownRule {
        amount: money(20_000),
        mode: TrailingDrawdownMode::Intraday,
        floor_stops_at: None,
    };
    service
        .register_risk_profile(prop_firm_profile(1, None, Some(rule)))
        .expect("profile registers");
    assert_eq!(
        equity_peak(&service),
        Some(money(5_000_000)),
        "registration starts the peak at the current equity"
    );
    buy_one_at_one_hundred(&service, &feed, &clock);
    quote_at(&service, &feed, &clock, "2026-10-16T12:00:03Z", 3, 10_300);
    assert_eq!(equity_peak(&service), Some(money(5_015_000)));
    let raised = meter(&service);
    assert_eq!(raised.maximum_drawdown_peak, Some(money(5_015_000)));
    assert_eq!(raised.maximum_drawdown_floor, Some(money(4_995_000)));
    assert_eq!(raised.maximum_drawdown_remaining, Some(money(20_000)));

    quote_at(&service, &feed, &clock, "2026-10-16T12:00:04Z", 4, 9_950);
    assert!(
        lock_kinds(&service).is_empty(),
        "$49,975.00 is above the floor"
    );
    let mut revision = prop_firm_profile(2, None, Some(rule));
    revision.daily_loss_limit = money(90_000);
    service
        .register_risk_profile(revision)
        .expect("revision registers");
    assert_eq!(
        equity_peak(&service),
        Some(money(5_015_000)),
        "a revision that keeps the rule keeps its peak"
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("service stops");

    let (restarted, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    open_session_feed(&feed, &instrument());
    restarted.status().expect("reopened feed drains");
    assert_eq!(
        equity_peak(&restarted),
        Some(money(5_015_000)),
        "the peak is durable across a store reopen"
    );
    quote_at(&restarted, &feed, &clock, "2026-10-16T12:00:06Z", 5, 9_900);
    assert_eq!(lock_kinds(&restarted), [RiskLockKind::MaximumDrawdown]);
    let locked = restarted.snapshot().expect("locked snapshot");
    assert_eq!(locked.risk_locks[0].reason, "maximum drawdown reached");
    assert_eq!(
        locked.risk_meters[0].maximum_drawdown_remaining,
        Some(money(0))
    );

    tick_timer_at(
        &restarted,
        &clock,
        rollover_wall_nanos("2026-10-17T00:00:01Z"),
    );
    let rolled = restarted.snapshot().expect("rolled snapshot");
    assert_eq!(
        rolled.risk_sessions[0].session_start_unix_nanos,
        rollover_wall_nanos("2026-10-17T00:00:00Z")
    );
    assert_eq!(
        lock_kinds(&restarted),
        [RiskLockKind::MaximumDrawdown],
        "the session boundary never releases the maximum drawdown"
    );
    assert_eq!(
        equity_peak(&restarted),
        Some(money(5_015_000)),
        "a lower boundary equity leaves the peak"
    );
    restarted
        .unlock_account(account())
        .expect("explicit unlock");
    assert_eq!(lock_kinds(&restarted), Vec::<RiskLockKind>::new());
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("restarted service stops");
}

#[test]
fn an_end_of_day_maximum_drawdown_moves_only_at_rollovers_and_stops_trailing() {
    let directory = TestDirectory::new("maximum-drawdown-end-of-day");
    let (service, clock, feed) = prop_firm_world(&directory);
    service
        .register_risk_profile(prop_firm_profile(
            1,
            None,
            Some(MaximumDrawdownRule {
                amount: money(20_000),
                mode: TrailingDrawdownMode::EndOfDay,
                floor_stops_at: Some(money(5_000_000)),
            }),
        ))
        .expect("profile registers");
    assert_eq!(
        meter(&service).maximum_drawdown_floor,
        Some(money(4_980_000))
    );
    buy_one_at_one_hundred(&service, &feed, &clock);
    quote_at(&service, &feed, &clock, "2026-10-16T12:00:03Z", 3, 10_300);
    assert_eq!(
        equity_peak(&service),
        Some(money(5_000_000)),
        "an end-of-day peak ignores intraday highs"
    );

    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-10-17T00:00:01Z"),
    );
    assert_eq!(
        equity_peak(&service),
        Some(money(5_015_000)),
        "the rollover raises the peak to the boundary equity"
    );
    assert_eq!(
        meter(&service).maximum_drawdown_floor,
        Some(money(4_995_000))
    );

    quote_at(&service, &feed, &clock, "2026-10-17T00:00:02Z", 4, 10_500);
    assert_eq!(equity_peak(&service), Some(money(5_015_000)));
    tick_timer_at(
        &service,
        &clock,
        rollover_wall_nanos("2026-10-18T00:00:01Z"),
    );
    let stopped = meter(&service);
    assert_eq!(stopped.maximum_drawdown_peak, Some(money(5_025_000)));
    assert_eq!(
        stopped.maximum_drawdown_floor,
        Some(money(5_000_000)),
        "the floor stops trailing at the starting balance instead of $50,050.00"
    );

    quote_at(&service, &feed, &clock, "2026-10-18T00:00:02Z", 5, 10_050);
    assert!(
        lock_kinds(&service).is_empty(),
        "$50,025.00 stays above the stopped floor"
    );
    quote_at(&service, &feed, &clock, "2026-10-18T00:00:03Z", 6, 10_000);
    assert_eq!(lock_kinds(&service), [RiskLockKind::MaximumDrawdown]);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("service stops");
}

#[test]
fn prop_firm_rules_are_validated_and_the_maximum_drawdown_is_practice_only() {
    let directory = TestDirectory::new("prop-firm-validation");
    let service = start_service(&directory);
    let rule = MaximumDrawdownRule {
        amount: money(20_000),
        mode: TrailingDrawdownMode::Intraday,
        floor_stops_at: None,
    };
    let refusals = [
        (
            prop_firm_profile(1, Some(money(0)), None),
            "daily profit target",
        ),
        (
            prop_firm_profile(
                1,
                Some(FixedPoint::try_new(10_000, 3).expect("target")),
                None,
            ),
            "risk profile currency scales do not match",
        ),
        (
            prop_firm_profile(
                1,
                None,
                Some(MaximumDrawdownRule {
                    amount: money(-1),
                    ..rule
                }),
            ),
            "maximum drawdown",
        ),
        (
            prop_firm_profile(
                1,
                None,
                Some(MaximumDrawdownRule {
                    amount: FixedPoint::try_new(20_000, 3).expect("amount"),
                    ..rule
                }),
            ),
            "risk profile currency scales do not match",
        ),
        (
            prop_firm_profile(
                1,
                None,
                Some(MaximumDrawdownRule {
                    floor_stops_at: Some(money(0)),
                    ..rule
                }),
            ),
            "maximum drawdown floor limit",
        ),
    ];
    for (profile, expected) in refusals {
        let error = service
            .register_risk_profile(profile)
            .expect_err("an invalid prop-firm rule is refused");
        assert!(error.contains(expected), "{error} names {expected}");
    }

    let mut broker_profile = prop_firm_profile(1, None, Some(rule));
    let broker = TradingAccount {
        id: TradingAccountId::try_new("broker-fixture").expect("id"),
        display_name: "Broker fixture".into(),
        environment: AccountEnvironment::Demo,
        venue_id: aeris_trading::venue::BROKER_VENUE_ID.into(),
        broker_ref: Some("fixture-ref".into()),
        currency: "USD".into(),
        currency_scale: 2,
        starting_equity: None,
    };
    service.register_account(broker.clone()).expect("broker");
    broker_profile.account_id = broker.id;
    assert_eq!(
        service
            .register_risk_profile(broker_profile)
            .expect_err("broker accounts carry no maximum drawdown"),
        "a maximum drawdown is available only for practice accounts"
    );
    assert!(
        service
            .snapshot()
            .expect("snapshot")
            .risk_profiles
            .is_empty(),
        "no refused profile is stored"
    );
    service
        .register_risk_profile(prop_firm_profile(1, Some(money(10_000)), Some(rule)))
        .expect("a valid practice profile registers");
    service
        .shutdown_after_owner_exit_for_test()
        .expect("service stops");
}
