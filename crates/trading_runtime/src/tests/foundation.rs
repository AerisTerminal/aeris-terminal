use super::*;

fn broker_account(environment: AccountEnvironment) -> TradingAccount {
    TradingAccount {
        id: TradingAccountId::try_new("broker-fixture").expect("id"),
        display_name: "Broker fixture".into(),
        environment,
        venue_id: aeris_trading::venue::BROKER_VENUE_ID.into(),
        broker_ref: Some("fixture-ref".into()),
        currency: "USD".into(),
        currency_scale: 2,
        starting_equity: None,
    }
}

#[test]
fn account_routes_and_broker_identity_survive_restart() {
    let directory = TestDirectory::new("venue-route");
    let service = start_service(&directory);
    service
        .register_account(broker_account(AccountEnvironment::Demo))
        .expect("register");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(
        VenueRoute::from_account(&snapshot.accounts[0]),
        Ok(VenueRoute::Simulated)
    );
    assert_eq!(
        VenueRoute::from_account(&snapshot.accounts[1]),
        Ok(VenueRoute::BrokerDemo)
    );
    let mut request = market_order("demo-route", OrderSide::Buy, 1, 1_000);
    request.account_id = snapshot.accounts[1].id.clone();
    assert_eq!(
        service.place_order(request).unwrap_err(),
        "broker demo venue is not connected"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let restarted = TradingService::start(config(&directory)).expect("restart");
    assert!(
        restarted
            .snapshot()
            .expect("snapshot")
            .accounts
            .contains(&broker_account(AccountEnvironment::Demo))
    );
    restarted
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
}

#[test]
fn broker_account_of_a_retired_venue_loads_but_never_routes() {
    let directory = TestDirectory::new("retired-venue");
    start_service(&directory)
        .shutdown(Duration::from_secs(2))
        .expect("shutdown");
    let database = rusqlite::Connection::open(&config(&directory).database_path).expect("database");
    database
        .execute(
            "INSERT INTO accounts(id,display_name,environment,currency,currency_scale,
                venue_id,broker_ref) VALUES ('retired-demo','Retired demo','demo','USD',2,
                'retired-venue','retired-ref')",
            [],
        )
        .expect("legacy broker account");
    drop(database);
    let service = TradingService::start(config(&directory)).expect("restart");
    let snapshot = service.snapshot().expect("snapshot");
    let retired = snapshot
        .accounts
        .iter()
        .find(|account| account.id.as_str() == "retired-demo")
        .expect("legacy account loads");
    assert_eq!(
        VenueRoute::from_account(retired),
        Err("trading account venue is not supported".into())
    );
    let mut request = market_order("retired-order", OrderSide::Buy, 1, 1_000);
    request.account_id = retired.id.clone();
    assert_eq!(
        service.place_order(request).unwrap_err(),
        "trading account venue is not supported"
    );
    let (_generation, requests) = service
        .attach_demo_venue(super::VENUE_CAPABILITIES)
        .expect("venue");
    assert!(
        requests.try_recv().is_err(),
        "a retired venue's account is never reconciled"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn account_venue_environment_and_broker_reference_must_agree() {
    let directory = TestDirectory::new("invalid-venue");
    let service = start_service(&directory);
    let mut account = broker_account(AccountEnvironment::Demo);
    account.broker_ref = None;
    assert_eq!(
        service.register_account(account.clone()).unwrap_err(),
        "trading account venue, environment and broker reference do not match"
    );
    account.broker_ref = Some("fixture".into());
    account.venue_id = "aeris-sim".into();
    assert_eq!(
        service.register_account(account).unwrap_err(),
        "trading account venue, environment and broker reference do not match"
    );
    assert_eq!(service.snapshot().expect("snapshot").accounts.len(), 1);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn broker_client_order_id_is_bounded_before_dispatch() {
    let directory = TestDirectory::new("client-id-bound");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    let broker = broker_account(AccountEnvironment::Demo);
    service.register_account(broker.clone()).expect("register");
    let (_generation, _requests) = service
        .attach_demo_venue(super::VENUE_CAPABILITIES)
        .expect("venue");
    let limit = super::VENUE_CAPABILITIES.client_order_id_limit;
    let mut request = market_order(&"x".repeat(limit + 1), OrderSide::Buy, 1, 1_000);
    request.account_id = broker.id.clone();
    assert_eq!(
        service.place_order(request).unwrap_err(),
        aeris_trading::RejectReason::ClientIdTooLong.to_string()
    );
    assert_eq!(service.snapshot().expect("snapshot").orders.len(), 0);
    let mut valid = market_order(&"x".repeat(limit), OrderSide::Buy, 2, 2_000);
    valid.account_id = broker.id;
    assert_eq!(
        service.place_order(valid).expect("at the limit").status,
        OrderStatus::Pending
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let database = rusqlite::Connection::open(&config(&directory).database_path).expect("database");
    let longest: i64 = database
        .query_row(
            "SELECT MAX(LENGTH(client_order_id)) FROM broker_orders",
            [],
            |row| row.get(0),
        )
        .expect("broker order ids");
    assert_eq!(longest, i64::try_from(limit).expect("limit"));
}

#[test]
fn live_account_rejects_every_trading_command_without_mutation() {
    let directory = TestDirectory::new("live-reject");
    let (service, _clock, feed) = live_feed_service(&directory);
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service.status().expect("owner drains verified terms");
    let mut request = market_order("live-order", OrderSide::Buy, 1, 1_000);
    let simulated = request.account_id.clone();
    service.place_order(request.clone()).expect("sim order");
    let mut broker = broker_account(AccountEnvironment::Live);
    broker.id = simulated;
    service
        .register_account(broker.clone())
        .expect("replace account");
    request.account_id = broker.id.clone();
    let expected = "broker live accounts are data-only; trading is disabled";
    assert_eq!(service.place_order(request).unwrap_err(), expected);
    assert_eq!(
        service
            .modify_order(ModifyOrder {
                client_order_id: ClientOrderId::try_new("live-order").expect("id"),
                time_in_force: TimeInForce::Day,
                limit_price: None,
                stop_price: None,
                modified_unix_nanos: 2_000,
                provenance: provenance(2, 2_000),
            })
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .cancel_order(ClientOrderId::try_new("live-order").expect("id"))
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .move_bracket_level(MoveBracketLevel {
                client_order_id: ClientOrderId::try_new("live-order").expect("id"),
                side: aeris_trading::venue::BracketSide::Stop,
                current_ticks: 8,
                new_ticks: 12,
            })
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .flatten(FlattenScope {
                account: Some(broker.id.clone()),
                instrument: None,
            })
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .reverse_position_at(broker.id.clone(), instrument().instrument_id)
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .kill_switch(Some(broker.id.clone()), "test".into(), 2_000)
            .unwrap_err(),
        expected
    );
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.orders.len(), 1);
    assert_eq!(snapshot.risk_locks.len(), 0);
    assert_eq!(snapshot.orders[0].status, OrderStatus::Working);

    // Global commands skip a data-only account instead of failing for every account; here
    // it is the only account, so they change nothing.
    assert!(
        service
            .flatten(FlattenScope::default())
            .expect("flatten all")
            .incomplete
            .is_none()
    );
    assert_eq!(
        service
            .kill_switch(None, "test".into(), 2_000)
            .expect("global kill switch"),
        EmergencyOutcome::default()
    );
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.risk_locks, []);
    assert_eq!(snapshot.orders[0].status, OrderStatus::Working);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn owner_replaces_desktop_venue_identity_on_simulated_place_and_modify() {
    let directory = TestDirectory::new("owner-provenance");
    let (service, _clock) = routed_fixture(
        &directory,
        &instrument(),
        market_feed::route(),
        10_000,
        10_025,
    );
    let mut request = market_order("route-identity", OrderSide::Buy, 1, 1_000);
    request.order_type = OrderType::Limit;
    request.time_in_force = TimeInForce::GoodTillCancelled;
    request.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    request.provenance.venue_id = "spoofed-venue".into();
    let placed = service
        .place_order(request)
        .expect("placed on simulated route");
    assert_eq!(placed.provenance.venue_id, "aeris-sim");
    let mut modified = ModifyOrder {
        client_order_id: placed.client_order_id,
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(FixedPoint::try_new(9_025, 2).expect("price")),
        stop_price: None,
        modified_unix_nanos: 2_000,
        provenance: provenance(2, 2_000),
    };
    modified.provenance.venue_id = "spoofed-venue".into();
    assert_eq!(
        service
            .modify_order(modified)
            .expect("modified")
            .provenance
            .venue_id,
        "aeris-sim"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn flatten_outcome_preserves_simulated_fills_with_no_pending_closes() {
    let directory = TestDirectory::new("flatten-outcome");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("id");
    assert_eq!(
        service
            .flatten(FlattenScope {
                account: Some(account),
                instrument: None,
            })
            .expect("flatten"),
        FlattenOutcome {
            fills: vec![],
            pending_exits: vec![],
            incomplete: None,
            unpriced: vec![],
        }
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn broker_brackets_are_venue_held_and_managed_rules_reject_without_state() {
    let directory = TestDirectory::new("managed-broker");
    let (service, _clock, feed) = live_feed_service(&directory);
    bootstrap_live_feed(&feed, &instrument(), market_feed::route());
    service.status().expect("owner drains verified terms");
    let broker = broker_account(AccountEnvironment::Demo);
    service.register_account(broker.clone()).expect("broker");
    let (_generation, _requests) = service
        .attach_demo_venue(super::VENUE_CAPABILITIES)
        .expect("venue");
    for (index, feature) in ["bracket", "trailing", "break-even"].iter().enumerate() {
        let mut template = bracket_template();
        template.template_id = format!("foundation-{feature}");
        if *feature == "bracket" {
            template.trailing_stop = None;
            template.break_even = None;
        } else if *feature == "trailing" {
            template.break_even = None;
        } else {
            template.trailing_stop = None;
        }
        let mut entry = market_order(
            &format!("broker-{feature}"),
            OrderSide::Buy,
            index as u64 + 1,
            1_000,
        );
        entry.account_id = broker.id.clone();
        entry.quantity = FixedPoint::try_new(2, 0).expect("quantity");
        let placed = service.place_inline_bracket(PlaceInlineBracket {
            entry,
            template: template.clone(),
        });
        match *feature {
            "trailing" => assert_eq!(
                placed.unwrap_err(),
                "managed trailing stops are unavailable for broker accounts"
            ),
            "break-even" => assert_eq!(
                placed.unwrap_err(),
                "managed break-even stops are unavailable for broker accounts"
            ),
            _ => assert!(matches!(
                placed,
                Ok(crate::PlacementOutcome::BrokerProtected(_))
            )),
        }
        let mut simulated = market_order(
            &format!("sim-{feature}"),
            OrderSide::Buy,
            index as u64 + 1,
            1_000,
        );
        simulated.quantity = FixedPoint::try_new(2, 0).expect("quantity");
        service
            .place_inline_bracket(PlaceInlineBracket {
                entry: simulated,
                template,
            })
            .expect("simulated feature");
    }
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.managed_brackets.len(), 3);
    // Each simulated bracket holds its entry plus a real inactive stop and two targets.
    assert_eq!(
        snapshot
            .orders
            .iter()
            .filter(|order| order.status == OrderStatus::Inactive)
            .count(),
        9
    );
    assert_eq!(snapshot.orders.len(), 13);
    assert_eq!(
        snapshot
            .orders
            .iter()
            .filter(|order| order.account_id == broker.id)
            .count(),
        1,
        "the broker bracket is one entry; the venue holds its levels"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn copier_rejects_broker_source_and_target_before_persisting() {
    let directory = TestDirectory::new("copier-broker");
    let service = start_service(&directory);
    let broker = broker_account(AccountEnvironment::Demo);
    service.register_account(broker.clone()).expect("broker");
    let simulated = TradingAccountId::try_new("aeris-sim-1").expect("sim");
    let config = |source_account_id, target_account_id| TradeCopierConfig {
        source_account_id,
        revision: 1,
        enabled: true,
        targets: vec![TradeCopierTarget {
            account_id: target_account_id,
            quantity_multiplier: FixedPoint::try_new(1, 0).expect("multiplier"),
            enabled: true,
        }],
    };
    assert_eq!(
        service
            .register_trade_copier(config(broker.id.clone(), simulated.clone()))
            .unwrap_err(),
        "trade copier is unavailable for broker accounts"
    );
    assert_eq!(
        service
            .register_trade_copier(config(simulated.clone(), broker.id))
            .unwrap_err(),
        "trade copier is unavailable for broker accounts"
    );
    assert_eq!(service.snapshot().expect("snapshot").trade_copiers.len(), 0);
    let other = service
        .create_practice_account(CreatePracticeAccount {
            display_name: "Target".into(),
            starting_equity: FixedPoint::try_new(100_000, 2).expect("equity"),
        })
        .expect("sim target");
    service
        .register_trade_copier(config(simulated, other.id.clone()))
        .expect("simulated copier");
    assert_eq!(service.snapshot().expect("snapshot").trade_copiers.len(), 1);
    let mut reclassified = broker_account(AccountEnvironment::Demo);
    reclassified.id = other.id;
    service
        .register_account(reclassified)
        .expect("target reclassified");
    let error = service
        .place_order(market_order(
            "copier-reclassified",
            OrderSide::Buy,
            1,
            1_000,
        ))
        .unwrap_err();
    assert_eq!(error, "trade copier is unavailable for broker accounts");
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.orders.len(), 0);
    assert_eq!(snapshot.copy_dispatches.len(), 0);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn economic_event_flatten_rejects_broker_and_preserves_simulated_rule() {
    let directory = TestDirectory::new("economic-broker");
    let service = start_service(&directory);
    let broker = broker_account(AccountEnvironment::Demo);
    service.register_account(broker.clone()).expect("broker");
    let profile = |account_id| RiskProfile {
        account_id,
        profile_id: "event-flatten".into(),
        version: 1,
        session_start_unix_nanos: 1,
        session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
        daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("daily"),
        trailing_drawdown: None,
        trailing_mode: TrailingDrawdownMode::Intraday,
        max_contracts: FixedPoint::try_new(5, 0).expect("contracts"),
        consistency_max_single_trade_percent: None,
        restricted_until_unix_nanos: None,
        economic_event_rule: Some(EconomicEventRiskRule {
            action: EconomicEventRiskAction::Flatten,
            minimum_importance: EconomicEventRiskImportance::High,
            lead_seconds: 300,
        }),
        enabled: true,
    };
    assert_eq!(
        service
            .register_risk_profile(profile(broker.id))
            .unwrap_err(),
        "economic-event flatten is unavailable for broker accounts"
    );
    assert_eq!(service.snapshot().expect("snapshot").risk_profiles.len(), 0);
    service
        .register_instrument(instrument())
        .expect("instrument");
    let simulated = TradingAccountId::try_new("aeris-sim-1").expect("sim");
    service
        .register_risk_profile(profile(simulated))
        .expect("sim rule");
    let event = EconomicEventRiskTrigger {
        event_id: "fixture-event".into(),
        title: "Event".into(),
        source: "Fixture".into(),
        importance: EconomicEventRiskImportance::High,
        scheduled_unix_nanos: 1_000_000_000_000,
        source_release_unix_nanos: 900_000_000_000,
        observed_unix_nanos: 950_000_000_000,
    };
    let result = service
        .apply_economic_event_risk(event)
        .expect("simulated rule");
    assert_eq!(result.flattened_accounts, 1);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}
