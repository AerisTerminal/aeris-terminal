use super::*;

fn broker_account(environment: AccountEnvironment) -> TradingAccount {
    TradingAccount {
        id: TradingAccountId::try_new("broker-fixture").expect("id"),
        display_name: "cTrader fixture".into(),
        environment,
        venue_id: "ctrader".into(),
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
        Ok(VenueRoute::CtraderDemo)
    );
    let mut request = market_order("demo-route", OrderSide::Buy, 1, 1_000);
    request.account_id = snapshot.accounts[1].id.clone();
    assert_eq!(
        service.place_order(request).unwrap_err(),
        "cTrader demo venue is not connected"
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
    let broker = broker_account(AccountEnvironment::Demo);
    service.register_account(broker.clone()).expect("register");
    let mut request = market_order(&"x".repeat(51), OrderSide::Buy, 1, 1_000);
    request.account_id = broker.id;
    assert_eq!(
        service.place_order(request).unwrap_err(),
        "cTrader ClientOrderId must be at most 50 characters"
    );
    assert_eq!(service.snapshot().expect("snapshot").orders.len(), 0);
    let mut valid = market_order(&"x".repeat(50), OrderSide::Buy, 2, 2_000);
    valid.account_id = broker_account(AccountEnvironment::Demo).id;
    assert_eq!(
        service.place_order(valid).unwrap_err(),
        DEMO_VENUE_UNAVAILABLE
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
    let database = rusqlite::Connection::open(&config(&directory).database_path).expect("database");
    let count: i64 = database
        .query_row("SELECT COUNT(*) FROM broker_orders", [], |row| row.get(0))
        .expect("broker order count");
    assert_eq!(count, 0);
}

#[test]
fn live_account_rejects_every_trading_command_without_mutation() {
    let directory = TestDirectory::new("live-reject");
    let service = start_service(&directory);
    let mut request = market_order("live-order", OrderSide::Buy, 1, 1_000);
    let simulated = request.account_id.clone();
    service
        .register_instrument(instrument())
        .expect("instrument");
    service.place_order(request.clone()).expect("sim order");
    let mut broker = broker_account(AccountEnvironment::Live);
    broker.id = simulated;
    service
        .register_account(broker.clone())
        .expect("replace account");
    request.account_id = broker.id.clone();
    let expected = "cTrader live accounts are data-only; trading is disabled";
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
            .close_broker_position(broker.id.clone(), "position".into())
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .amend_broker_position_sltp(broker.id.clone(), "position".into(), None, None)
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .flatten_account(broker.id.clone(), observation(10_000, 10_025, 2, 2_000))
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .reverse_position(broker.id.clone(), observation(10_000, 10_025, 2, 2_000))
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .kill_switch(Some(broker.id), "test".into(), 2_000)
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service
            .flatten_all(observation(10_000, 10_025, 2, 2_000))
            .unwrap_err(),
        expected
    );
    assert_eq!(
        service.kill_switch(None, "test".into(), 2_000).unwrap_err(),
        expected
    );
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(snapshot.orders.len(), 1);
    assert_eq!(snapshot.risk_locks.len(), 0);
    assert_eq!(snapshot.orders[0].status, OrderStatus::Working);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn owner_replaces_desktop_venue_identity_on_simulated_place_and_modify() {
    let directory = TestDirectory::new("owner-provenance");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
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
            .flatten_account(account, observation(10_000, 10_025, 1, 1_000))
            .expect("flatten"),
        FlattenOutcome {
            fills: vec![],
            pending_close_requests: vec![]
        }
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn managed_brackets_trailing_and_break_even_reject_broker_without_state() {
    let directory = TestDirectory::new("managed-broker");
    let service = start_service(&directory);
    service
        .register_instrument(instrument())
        .expect("instrument");
    let broker = broker_account(AccountEnvironment::Demo);
    service.register_account(broker.clone()).expect("broker");
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
        assert_eq!(
            service
                .place_inline_bracket(PlaceInlineBracket {
                    entry,
                    template: template.clone()
                })
                .unwrap_err(),
            match *feature {
                "trailing" => "managed trailing stops are unavailable for broker accounts",
                "break-even" => "managed break-even stops are unavailable for broker accounts",
                _ => "managed brackets are unavailable for broker accounts",
            }
        );
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
    assert_eq!(snapshot.orders.len(), 3);
    assert!(
        snapshot
            .orders
            .iter()
            .all(|order| order.account_id != broker.id)
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
        .apply_economic_event_risk(event, Some(observation(10_000, 10_025, 1, 950_000_000_000)))
        .expect("simulated rule");
    assert_eq!(result.flattened_accounts, 1);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}
