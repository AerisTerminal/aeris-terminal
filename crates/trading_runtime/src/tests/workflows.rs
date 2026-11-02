use super::*;
use crate::clock::Clock;

fn account() -> TradingAccountId {
    TradingAccountId::try_new("aeris-sim-1").expect("account")
}

fn price(units: i64) -> FixedPoint {
    FixedPoint::try_new(units, 2).expect("price")
}

fn one() -> FixedPoint {
    FixedPoint::try_new(1, 0).expect("quantity")
}

const IF_NODE: WorkflowNodeId = WorkflowNodeId(1);
const THEN_NODE: WorkflowNodeId = WorkflowNodeId(2);
const ELSE_NODE: WorkflowNodeId = WorkflowNodeId(3);

fn bracketed_buy() -> WorkflowAction {
    WorkflowAction::Enter {
        side: OrderSide::Buy,
        quantity: one(),
        protection: Some(WorkflowProtection {
            stop_ticks: 8,
            target_ticks: 16,
        }),
    }
}

fn action_node(id: WorkflowNodeId, action: WorkflowAction) -> WorkflowNode {
    WorkflowNode {
        id,
        step: WorkflowStep::Action { action, next: None },
    }
}

/// When the price crosses above `level`: if flat, buy one with a bracket.
fn breakout(workflow_id: &str, level: i64) -> WorkflowDefinition {
    WorkflowDefinition {
        workflow_id: workflow_id.into(),
        revision: 1,
        name: "Breakout long".into(),
        account_id: account(),
        scope: WorkflowScope::Instrument(instrument().instrument_id),
        triggers: vec![WorkflowTrigger::PriceCrosses {
            direction: PriceDirection::Above,
            level: price(level),
        }],
        start: IF_NODE,
        nodes: vec![
            WorkflowNode {
                id: IF_NODE,
                step: WorkflowStep::If {
                    condition: WorkflowCondition::single(WorkflowCheck::Position(
                        PositionRequirement::Flat,
                    )),
                    then: Some(THEN_NODE),
                    otherwise: None,
                },
            },
            action_node(THEN_NODE, bracketed_buy()),
        ],
        limits: WorkflowLimits {
            repeat: WorkflowRepeat::Once,
            cooldown_seconds: 0,
            expires_unix_nanos: None,
        },
    }
}

/// The same trigger running a single action.
fn single(workflow_id: &str, level: i64, action: WorkflowAction) -> WorkflowDefinition {
    WorkflowDefinition {
        start: THEN_NODE,
        nodes: vec![action_node(THEN_NODE, action)],
        ..breakout(workflow_id, level)
    }
}

fn register(service: &TradingService, definition: WorkflowDefinition) -> WorkflowView {
    service
        .register_workflow(definition, WorkflowLayout::default())
        .expect("accepted")
}

fn working_set(service: &TradingService) -> WorkingSet {
    service.request(Command::WorkingSet).expect("working set")
}

fn workflow(service: &TradingService, workflow_id: &str) -> WorkflowView {
    working_set(service)
        .workflows
        .into_iter()
        .find(|view| view.definition.workflow_id == workflow_id)
        .expect("workflow is published")
}

fn executed(view: &WorkflowView) -> Vec<WorkflowStepResult> {
    match &view.recent_firings[0].outcome {
        WorkflowFiringOutcome::Executed(steps) => steps.clone(),
        outcome => panic!("executed: {outcome:?}"),
    }
}

fn resting_limit(client_id: &str, time: i64) -> PlaceOrder {
    PlaceOrder {
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GoodTillCancelled,
        limit_price: Some(price(9_000)),
        ..market_order(client_id, OrderSide::Buy, 1, time)
    }
}

#[test]
fn a_price_cross_fires_once_and_enters_with_protection() {
    let directory = TestDirectory::new("workflow-cross");
    let instrument = instrument();
    let (service, _clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let registered = register(&service, breakout("es-breakout", 10_050));
    assert_eq!(registered.state, WorkflowState::Paused);
    let armed = service
        .arm_workflow("es-breakout".into(), 1)
        .expect("armed");
    assert_eq!(armed.state, WorkflowState::Armed);
    assert!(armed.blockers.is_empty(), "{:?}", armed.blockers);

    // The first quote after arming only records the side of the level.
    assert_eq!(
        deliver_quote(&service, &feed, &instrument, 2, 10_000, 10_025),
        []
    );
    assert_eq!(workflow(&service, "es-breakout").recent_firings, []);
    deliver_quote(&service, &feed, &instrument, 3, 10_050, 10_075);
    // A bracket entry waits for the quote after its acceptance, as a manual one does.
    let fills = deliver_quote(&service, &feed, &instrument, 4, 10_050, 10_075);
    assert_eq!(fills.len(), 1, "the entry fills at the ask");
    assert_eq!(fills[0].price, price(10_075));

    let view = workflow(&service, "es-breakout");
    assert_eq!(view.state, WorkflowState::Completed);
    assert_eq!(view.total_firings, 1);
    assert_eq!(view.recent_firings.len(), 1);
    let steps = executed(&view);
    let [
        WorkflowStepResult {
            node: IF_NODE,
            outcome: WorkflowStepOutcome::Branch(true),
        },
        WorkflowStepResult {
            node: THEN_NODE,
            outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Placed { client_order_id }),
        },
    ] = steps.as_slice()
    else {
        panic!("if flat, then placed: {steps:?}");
    };
    let set = working_set(&service);
    assert!(
        set.managed_brackets
            .iter()
            .any(|bracket| &bracket.entry_client_order_id == client_order_id),
        "the entry carries its stop and target"
    );
    assert_eq!(set.positions.len(), 1);

    // A completed workflow never fires again.
    deliver_quote(&service, &feed, &instrument, 5, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 6, 10_050, 10_075);
    assert_eq!(workflow(&service, "es-breakout").recent_firings.len(), 1);
    assert_eq!(
        service.arm_workflow("es-breakout".into(), 1),
        Err(WorkflowError::Completed)
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn the_false_branch_runs_its_own_actions() {
    let directory = TestDirectory::new("workflow-else");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    service
        .place_order(resting_limit("resting-buy", clock.wall_unix_nanos()), false)
        .expect("resting order");
    // If long, flatten; otherwise cancel the working orders.
    let mut definition = breakout("long-or-cancel", 10_050);
    definition.nodes = vec![
        WorkflowNode {
            id: IF_NODE,
            step: WorkflowStep::If {
                condition: WorkflowCondition::single(WorkflowCheck::Position(
                    PositionRequirement::Long,
                )),
                then: Some(THEN_NODE),
                otherwise: Some(ELSE_NODE),
            },
        },
        action_node(THEN_NODE, WorkflowAction::Flatten),
        action_node(ELSE_NODE, WorkflowAction::CancelOrders),
    ];
    register(&service, definition);
    service
        .arm_workflow("long-or-cancel".into(), 1)
        .expect("armed");
    deliver_quote(&service, &feed, &instrument, 2, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 3, 10_050, 10_075);

    assert_eq!(
        executed(&workflow(&service, "long-or-cancel")),
        vec![
            WorkflowStepResult {
                node: IF_NODE,
                outcome: WorkflowStepOutcome::Branch(false),
            },
            WorkflowStepResult {
                node: ELSE_NODE,
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Cancelled { orders: 1 }),
            },
        ]
    );
    assert_eq!(working_set(&service).orders, []);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_branch_without_an_action_is_not_a_firing() {
    let directory = TestDirectory::new("workflow-empty-branch");
    let instrument = instrument();
    let (service, _clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    // Only while the bid is at or above 101.00, which it is not at the first cross.
    let mut definition = breakout("dip-only", 10_050);
    definition.nodes[0].step = WorkflowStep::If {
        condition: WorkflowCondition::single(WorkflowCheck::Price {
            direction: PriceDirection::Above,
            level: price(10_100),
        }),
        then: Some(THEN_NODE),
        otherwise: None,
    };
    register(&service, definition);
    service.arm_workflow("dip-only".into(), 1).expect("armed");
    deliver_quote(&service, &feed, &instrument, 2, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 3, 10_050, 10_075);
    let view = workflow(&service, "dip-only");
    assert_eq!(view.recent_firings, []);
    assert_eq!(view.state, WorkflowState::Armed, "a once workflow waits on");
    assert_eq!(view.last_attempt_unix_nanos, None);

    // The bid then crosses above the condition's level too.
    deliver_quote(&service, &feed, &instrument, 4, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 5, 10_100, 10_125);
    let view = workflow(&service, "dip-only");
    assert_eq!(view.state, WorkflowState::Completed);
    assert!(matches!(
        executed(&view).as_slice(),
        [
            WorkflowStepResult {
                outcome: WorkflowStepOutcome::Branch(true),
                ..
            },
            WorkflowStepResult {
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Placed { .. }),
                ..
            },
        ]
    ));
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_time_trigger_cancels_working_orders_and_reschedules() {
    let directory = TestDirectory::new("workflow-time");
    let instrument = instrument();
    let (service, clock, _feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    service
        .place_order(resting_limit("resting-buy", clock.wall_unix_nanos()), false)
        .expect("resting order");
    // The fixture clock is 00:16:40 UTC; the trigger is due at 00:17.
    let due = 17 * 60 * 1_000_000_000;
    let mut definition = single("cancel-at-1700", 10_050, WorkflowAction::CancelOrders);
    definition.triggers = vec![WorkflowTrigger::TimeOfDay(WorkflowClockTime {
        zone: WorkflowTimeZone::Utc,
        minute_of_day: 17,
    })];
    definition.limits.repeat = WorkflowRepeat::PerSession { maximum: 3 };
    register(&service, definition);
    let armed = service
        .arm_workflow("cancel-at-1700".into(), 1)
        .expect("armed");
    assert_eq!(armed.next_due_unix_nanos, Some(due));

    clock.advance(Duration::from_secs(25));
    service.status().expect("owner runs the due timer");
    let view = workflow(&service, "cancel-at-1700");
    assert_eq!(view.state, WorkflowState::Armed);
    assert_eq!(
        executed(&view),
        vec![WorkflowStepResult {
            node: THEN_NODE,
            outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Cancelled { orders: 1 }),
        }]
    );
    assert_eq!(view.next_due_unix_nanos, Some(due + 86_400_000_000_000));
    assert_eq!(working_set(&service).orders, []);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn any_trigger_fires_and_an_interval_trigger_repeats() {
    let directory = TestDirectory::new("workflow-any-trigger");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let mut definition = single("cross-or-every", 10_050, WorkflowAction::CancelOrders);
    definition
        .triggers
        .push(WorkflowTrigger::Every { minutes: 5 });
    definition.limits.repeat = WorkflowRepeat::PerSession { maximum: 5 };
    register(&service, definition);
    // The fixture clock is 00:16:40 UTC; the next five-minute mark is 00:20.
    let mark = 20 * 60 * 1_000_000_000;
    let armed = service
        .arm_workflow("cross-or-every".into(), 1)
        .expect("armed");
    assert_eq!(armed.next_due_unix_nanos, Some(mark));

    deliver_quote(&service, &feed, &instrument, 2, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 3, 10_050, 10_075);
    assert_eq!(workflow(&service, "cross-or-every").total_firings, 1);

    clock.advance(Duration::from_secs(200));
    service.status().expect("owner runs the due timer");
    let view = workflow(&service, "cross-or-every");
    assert_eq!(view.total_firings, 2, "the interval fires as well");
    assert_eq!(
        view.next_due_unix_nanos,
        Some(mark + 5 * 60 * 1_000_000_000)
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_timed_bracket_entry_fills_on_the_next_quote() {
    let directory = TestDirectory::new("workflow-timed-bracket");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let mut definition = single("timed-entry", 10_050, bracketed_buy());
    definition.triggers = vec![WorkflowTrigger::Every { minutes: 1 }];
    register(&service, definition);
    service
        .arm_workflow("timed-entry".into(), 1)
        .expect("armed");
    clock.advance(Duration::from_secs(19));
    deliver_quote_at(
        &service,
        &feed,
        &instrument,
        2,
        9_975,
        10_000,
        clock.wall_unix_nanos(),
    );
    clock.advance(Duration::from_secs(1));
    service.status().expect("owner runs the due timer");
    assert_eq!(workflow(&service, "timed-entry").total_firings, 1);
    clock.advance(Duration::from_millis(100));
    let fills = deliver_quote_at(
        &service,
        &feed,
        &instrument,
        3,
        9_975,
        10_000,
        clock.wall_unix_nanos(),
    );
    assert_eq!(fills.len(), 1, "the timed entry fills on the next quote");
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_timed_bracket_entry_on_a_significant_figure_grid_fills_on_the_next_quote() {
    let directory = TestDirectory::new("workflow-timed-bracket-sf");
    let mut eth = high_precision_crypto_instrument();
    eth.instrument_id = InstrumentId::try_new("hyperliquid:perp:ETH").expect("instrument");
    eth.contract.tick_size = None;
    eth.contract.price_grid = aeris_instruments::PriceGrid::SignificantFigures {
        figures: 5,
        max_decimals: 2,
    };
    let route = aeris_trading::market::MarketRoute {
        provider_id: "fixture".into(),
        instrument_id: eth.instrument_id.clone(),
        provider_symbol: "ETH".into(),
        display_symbol: "ETH-USDC".into(),
        venue_id: "HYPERLIQUID".into(),
        entitlement_id: None,
        price_scale: 8,
        quantity_scale: 8,
    };
    let (service, clock, feed) = live_feed_service(&directory);
    bootstrap_always_open_feed(&feed, &eth, route);
    service.status().expect("verified terms drain");
    let quote = |sequence| {
        push_live_quote_at(
            &feed,
            &eth,
            sequence,
            228_840_000_000,
            228_850_000_000,
            8,
            clock.wall_unix_nanos(),
        );
        service.status().expect("owner applies quote");
    };
    quote(1);
    let mut definition = single(
        "timed-eth",
        10_050,
        WorkflowAction::Enter {
            side: OrderSide::Buy,
            quantity: FixedPoint::try_new(100_000_000, 8).expect("one"),
            protection: Some(WorkflowProtection {
                stop_ticks: 20,
                target_ticks: 40,
            }),
        },
    );
    definition.scope = WorkflowScope::Instrument(eth.instrument_id.clone());
    definition.triggers = vec![WorkflowTrigger::Every { minutes: 1 }];
    register(&service, definition);
    service.arm_workflow("timed-eth".into(), 1).expect("armed");
    clock.advance(Duration::from_secs(19));
    quote(2);
    clock.advance(Duration::from_secs(1));
    service.status().expect("owner runs the due timer");
    let view = workflow(&service, "timed-eth");
    assert_eq!(view.total_firings, 1, "{:?}", view.recent_firings);
    clock.advance(Duration::from_millis(100));
    quote(3);
    let set = working_set(&service);
    assert_eq!(
        set.positions.len(),
        1,
        "error {:?}, firings {:?}",
        set.market_observation_error,
        workflow(&service, "timed-eth").recent_firings
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_workflow_rearmed_after_its_quote_interest_lapsed_still_fills() {
    let directory = TestDirectory::new("workflow-rearmed-interest");
    let mut eth = high_precision_crypto_instrument();
    eth.instrument_id = InstrumentId::try_new("hyperliquid:perp:ETH").expect("instrument");
    let route = aeris_trading::market::MarketRoute {
        provider_id: "fixture".into(),
        instrument_id: eth.instrument_id.clone(),
        provider_symbol: "ETH".into(),
        display_symbol: "ETH-USDC".into(),
        venue_id: "HYPERLIQUID".into(),
        entitlement_id: None,
        price_scale: 8,
        quantity_scale: 8,
    };
    let (service, clock, feed) = live_feed_service(&directory);
    bootstrap_always_open_feed(&feed, &eth, route.clone());
    service.status().expect("verified terms drain");
    let mut sequence = 0;
    let mut quote = || {
        sequence += 1;
        push_live_quote_at(
            &feed,
            &eth,
            sequence,
            228_840_000_000,
            228_850_000_000,
            8,
            clock.wall_unix_nanos(),
        );
        service.status().expect("owner applies quote");
    };
    let mut definition = single(
        "rearmed-eth",
        10_050,
        WorkflowAction::Enter {
            side: OrderSide::Buy,
            quantity: FixedPoint::try_new(100_000_000, 8).expect("one"),
            protection: Some(WorkflowProtection {
                stop_ticks: 20,
                target_ticks: 40,
            }),
        },
    );
    definition.scope = WorkflowScope::Instrument(eth.instrument_id.clone());
    definition.triggers = vec![WorkflowTrigger::Every { minutes: 1 }];
    register(&service, definition);
    service
        .arm_workflow("rearmed-eth".into(), 1)
        .expect("armed");
    for _ in 0..5 {
        quote();
    }
    // Pausing drops the owner's only interest in the market; arming again restores it and
    // the market runtime republishes the route's feed state and session.
    service
        .pause_workflow("rearmed-eth".into(), 1)
        .expect("paused");
    service
        .arm_workflow("rearmed-eth".into(), 1)
        .expect("armed again");
    bootstrap_always_open_feed(&feed, &eth, route);
    service.status().expect("republished feed drains");
    quote();
    clock.advance(Duration::from_secs(19));
    quote();
    clock.advance(Duration::from_secs(1));
    service.status().expect("owner runs the due timer");
    assert_eq!(workflow(&service, "rearmed-eth").total_firings, 1);
    clock.advance(Duration::from_millis(100));
    quote();
    let set = working_set(&service);
    assert_eq!(
        set.positions.len(),
        1,
        "error {:?}, orders {:?}",
        set.market_observation_error,
        set.orders
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn joined_checks_read_the_quote_range_and_the_weekday() {
    let directory = TestDirectory::new("workflow-joined-checks");
    let instrument = instrument();
    let (service, _clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let in_range = WorkflowCheck::PriceRange {
        low: price(10_000),
        high: price(10_100),
    };
    let on = |day: u8| WorkflowCheck::Weekday {
        zone: WorkflowTimeZone::Utc,
        days: WorkflowWeekdays::from_bits(1 << day).expect("day"),
    };
    // The fixture clock is on 1970-01-01, a Thursday (3, counting Monday as 0).
    for (workflow_id, condition) in [
        (
            "thursday-in-range",
            WorkflowCondition::joined(WorkflowJoin::All, &[in_range, on(3)]),
        ),
        (
            "friday-in-range",
            WorkflowCondition::joined(WorkflowJoin::All, &[in_range, on(4)]),
        ),
        (
            "friday-or-in-range",
            WorkflowCondition::joined(WorkflowJoin::Any, &[on(4), in_range]),
        ),
    ] {
        let mut definition = breakout(workflow_id, 10_050);
        definition.nodes = vec![
            WorkflowNode {
                id: IF_NODE,
                step: WorkflowStep::If {
                    condition: condition.expect("condition"),
                    then: Some(THEN_NODE),
                    otherwise: Some(ELSE_NODE),
                },
            },
            action_node(THEN_NODE, WorkflowAction::CancelOrders),
            action_node(ELSE_NODE, WorkflowAction::CancelOrders),
        ];
        register(&service, definition);
        service.arm_workflow(workflow_id.into(), 1).expect("armed");
    }
    deliver_quote(&service, &feed, &instrument, 2, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 3, 10_050, 10_075);
    for (workflow_id, held) in [
        ("thursday-in-range", true),
        ("friday-in-range", false),
        ("friday-or-in-range", true),
    ] {
        assert_eq!(
            executed(&workflow(&service, workflow_id))[0].outcome,
            WorkflowStepOutcome::Branch(held),
            "{workflow_id}"
        );
    }
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_locked_account_blocks_the_firing_and_the_session_cap_holds() {
    let directory = TestDirectory::new("workflow-limits");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let mut definition = breakout("capped", 10_050);
    definition.limits.repeat = WorkflowRepeat::PerSession { maximum: 1 };
    register(&service, definition);
    service.arm_workflow("capped".into(), 1).expect("armed");
    service
        .lock_account(account(), "manual".into(), clock.wall_unix_nanos())
        .expect("locked");
    assert_eq!(
        workflow(&service, "capped").blockers,
        vec![WorkflowBlocker::AccountLocked]
    );

    deliver_quote(&service, &feed, &instrument, 2, 10_000, 10_025);
    assert_eq!(
        deliver_quote(&service, &feed, &instrument, 3, 10_050, 10_075),
        []
    );
    let view = workflow(&service, "capped");
    assert_eq!(
        view.recent_firings[0].outcome,
        WorkflowFiringOutcome::Blocked(vec![WorkflowBlocker::AccountLocked])
    );
    assert_eq!(
        view.session_firings, 0,
        "a blocked firing spends no session firing"
    );
    assert_eq!(working_set(&service).orders, []);

    service.unlock_account(account()).expect("unlocked");
    deliver_quote(&service, &feed, &instrument, 4, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 5, 10_050, 10_075);
    assert_eq!(
        deliver_quote(&service, &feed, &instrument, 6, 10_050, 10_075).len(),
        1
    );
    service
        .flatten(FlattenScope::default())
        .expect("flat again");
    deliver_quote(&service, &feed, &instrument, 7, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 8, 10_050, 10_075);
    assert!(
        working_set(&service).positions.is_empty(),
        "the session cap of one is spent"
    );
    let view = workflow(&service, "capped");
    assert_eq!(view.session_firings, 1);
    assert_eq!(view.recent_firings.len(), 2);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn revisions_are_fenced_and_every_new_revision_starts_paused() {
    let directory = TestDirectory::new("workflow-revisions");
    let instrument = instrument();
    let (service, _clock, _feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    register(&service, breakout("revisioned", 10_050));
    service.arm_workflow("revisioned".into(), 1).expect("armed");
    assert_eq!(
        service.register_workflow(breakout("revisioned", 10_100), WorkflowLayout::default()),
        Err(WorkflowError::RevisionMustAdvance { current: 1 })
    );
    let mut next = breakout("revisioned", 10_100);
    next.revision = 2;
    assert_eq!(register(&service, next).state, WorkflowState::Paused);
    assert_eq!(
        service.arm_workflow("revisioned".into(), 1),
        Err(WorkflowError::StaleRevision { current: 2 })
    );
    let mut stranger = breakout("stranger", 10_050);
    stranger.account_id = TradingAccountId::try_new("aeris-sim-9").expect("account");
    assert_eq!(
        service.register_workflow(stranger, WorkflowLayout::default()),
        Err(WorkflowError::UnknownAccount)
    );
    let mut off_scale = breakout("off-scale", 10_050);
    off_scale.nodes[0].step = WorkflowStep::If {
        condition: WorkflowCondition::single(WorkflowCheck::Price {
            direction: PriceDirection::Above,
            level: FixedPoint::try_new(100_500, 3).expect("level"),
        }),
        then: Some(THEN_NODE),
        otherwise: None,
    };
    assert_eq!(
        service.register_workflow(off_scale, WorkflowLayout::default()),
        Err(WorkflowError::Invalid(WorkflowInvalid::Level))
    );
    let mut cyclic = breakout("cyclic", 10_050);
    cyclic.nodes[1].step = WorkflowStep::Action {
        action: WorkflowAction::Flatten,
        next: Some(IF_NODE),
    };
    assert_eq!(
        service.register_workflow(cyclic, WorkflowLayout::default()),
        Err(WorkflowError::Invalid(WorkflowInvalid::Cycle))
    );
    service
        .delete_workflow("revisioned".into())
        .expect("deleted");
    assert_eq!(working_set(&service).workflows, []);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn moving_nodes_keeps_the_revision_and_survives_a_restart() {
    let directory = TestDirectory::new("workflow-layout");
    let instrument = instrument();
    let (service, clock, _feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let at = |node, x, y| WorkflowNodePosition { node, x, y };
    let placed = service
        .register_workflow(
            breakout("laid-out", 10_050),
            WorkflowLayout {
                positions: vec![at(WorkflowNodeId::TRIGGER, 0, 0)],
            },
        )
        .expect("accepted");
    service.arm_workflow("laid-out".into(), 1).expect("armed");
    let moved = WorkflowLayout {
        positions: vec![
            at(WorkflowNodeId::TRIGGER, -40, 20),
            at(IF_NODE, 260, 20),
            at(THEN_NODE, 520, -60),
        ],
    };
    let view = service
        .set_workflow_layout("laid-out".into(), 1, moved.clone())
        .expect("moved");
    assert_eq!(view.layout, moved);
    assert_eq!(view.definition, placed.definition);
    assert_eq!(view.state, WorkflowState::Armed, "moving never pauses");
    assert_eq!(
        service.set_workflow_layout(
            "laid-out".into(),
            1,
            WorkflowLayout {
                positions: vec![at(ELSE_NODE, 0, 0)],
            },
        ),
        Err(WorkflowError::Invalid(WorkflowInvalid::Layout))
    );
    assert_eq!(
        service.set_workflow_layout("laid-out".into(), 2, moved.clone()),
        Err(WorkflowError::StaleRevision { current: 1 })
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");

    let (restarted, _feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument, market_feed::route());
    let after = workflow(&restarted, "laid-out");
    assert_eq!(after.layout, moved);
    assert_eq!(after.definition.revision, 1);
    assert_eq!(after.state, WorkflowState::Armed);
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn workflows_and_their_firings_survive_a_restart() {
    let directory = TestDirectory::new("workflow-restart");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    // Every stored kind of trigger and check comes back exactly.
    let mut definition = breakout("durable", 10_050);
    definition
        .triggers
        .push(WorkflowTrigger::Every { minutes: 720 });
    definition.triggers.push(WorkflowTrigger::PositionOpened);
    definition.triggers.push(WorkflowTrigger::PositionClosed);
    definition.nodes = vec![
        WorkflowNode {
            id: IF_NODE,
            step: WorkflowStep::If {
                condition: WorkflowCondition::joined(
                    WorkflowJoin::Any,
                    &[
                        WorkflowCheck::PriceRange {
                            low: price(9_000),
                            high: price(20_000),
                        },
                        WorkflowCheck::Weekday {
                            zone: WorkflowTimeZone::Chicago,
                            days: WorkflowWeekdays::WEEKDAYS,
                        },
                        WorkflowCheck::TimeInTrade {
                            compare: WorkflowCompare::AtMost,
                            minutes: 90,
                        },
                        WorkflowCheck::OpenProfit {
                            compare: WorkflowCompare::AtLeast,
                            amount: FixedPoint::try_new(-2_500, 2).expect("amount"),
                        },
                    ],
                )
                .expect("condition"),
                then: Some(THEN_NODE),
                otherwise: Some(ELSE_NODE),
            },
        },
        action_node(THEN_NODE, WorkflowAction::CancelOrders),
        WorkflowNode {
            id: ELSE_NODE,
            step: WorkflowStep::If {
                condition: WorkflowCondition::joined(
                    WorkflowJoin::All,
                    &[
                        WorkflowCheck::OpenProfitTicks {
                            compare: WorkflowCompare::AtMost,
                            ticks: -8,
                        },
                        WorkflowCheck::Pullback { ticks: 6 },
                    ],
                )
                .expect("condition"),
                then: Some(WorkflowNodeId(4)),
                otherwise: None,
            },
        },
        action_node(WorkflowNodeId(4), WorkflowAction::Flatten),
    ];
    definition.limits.repeat = WorkflowRepeat::PerSession { maximum: 5 };
    register(&service, definition);
    service.arm_workflow("durable".into(), 1).expect("armed");
    deliver_quote(&service, &feed, &instrument, 2, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 3, 10_050, 10_075);
    let before = workflow(&service, "durable");
    assert_eq!(before.recent_firings.len(), 1);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");

    let (restarted, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument, market_feed::route());
    let after = workflow(&restarted, "durable");
    assert_eq!(after.definition, before.definition);
    assert_eq!(after.state, WorkflowState::Armed);
    assert_eq!(after.recent_firings, before.recent_firings);
    assert_eq!(after.session_firings, 1);
    // The first quote after the restart only records the side, even past the level.
    deliver_quote(&restarted, &feed, &instrument, 4, 10_050, 10_075);
    assert_eq!(workflow(&restarted, "durable").recent_firings.len(), 1);
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

/// On `trigger`, cancel the working orders, up to ten times a session.
fn position_watch(workflow_id: &str, trigger: WorkflowTrigger) -> WorkflowDefinition {
    let mut definition = single(workflow_id, 10_050, WorkflowAction::CancelOrders);
    definition.triggers = vec![trigger];
    definition.limits.repeat = WorkflowRepeat::PerSession { maximum: 10 };
    definition
}

/// Every minute, reads `check` and cancels the working orders on either branch, so each
/// firing records the branch the check decided.
fn probe(workflow_id: &str, check: WorkflowCheck) -> WorkflowDefinition {
    let mut definition = position_watch(workflow_id, WorkflowTrigger::Every { minutes: 1 });
    definition.start = IF_NODE;
    definition.nodes = vec![
        WorkflowNode {
            id: IF_NODE,
            step: WorkflowStep::If {
                condition: WorkflowCondition::single(check),
                then: Some(THEN_NODE),
                otherwise: Some(ELSE_NODE),
            },
        },
        action_node(THEN_NODE, WorkflowAction::CancelOrders),
        action_node(ELSE_NODE, WorkflowAction::CancelOrders),
    ];
    definition
}

fn branch_taken(service: &TradingService, workflow_id: &str) -> bool {
    match executed(&workflow(service, workflow_id)).first() {
        Some(WorkflowStepResult {
            outcome: WorkflowStepOutcome::Branch(held),
            ..
        }) => *held,
        steps => panic!("{workflow_id} decided a branch: {steps:?}"),
    }
}

/// Places a market order, which fills on the owner's current quote, then delivers the next
/// quote.
fn trade(
    service: &TradingService,
    clock: &crate::clock::ManualClock,
    feed: &crate::core::market::feed::MarketFeedInbox,
    order: (&str, OrderSide, i64),
    sequence: u64,
    book: (i64, i64),
) -> Vec<Fill> {
    trade_in(service, clock, feed, &instrument(), order, sequence, book)
}

/// [`trade`] in `instrument`.
fn trade_in(
    service: &TradingService,
    clock: &crate::clock::ManualClock,
    feed: &crate::core::market::feed::MarketFeedInbox,
    instrument: &TradingInstrument,
    (client_id, side, quantity): (&str, OrderSide, i64),
    sequence: u64,
    (bid, ask): (i64, i64),
) -> Vec<Fill> {
    let mut order = market_order(client_id, side, 1, clock.wall_unix_nanos());
    order.instrument_id = instrument.instrument_id.clone();
    order.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    service.place_order(order, false).expect("market order");
    clock.advance(Duration::from_millis(100));
    deliver_quote_at(
        service,
        feed,
        instrument,
        sequence,
        bid,
        ask,
        clock.wall_unix_nanos(),
    )
}

/// Delivers a quote received a moment after the clock's current time.
fn quote_now(
    service: &TradingService,
    clock: &crate::clock::ManualClock,
    feed: &crate::core::market::feed::MarketFeedInbox,
    sequence: u64,
    (bid, ask): (i64, i64),
) -> Vec<Fill> {
    clock.advance(Duration::from_millis(100));
    deliver_quote_at(
        service,
        feed,
        &instrument(),
        sequence,
        bid,
        ask,
        clock.wall_unix_nanos(),
    )
}

#[test]
fn position_triggers_fire_when_a_position_opens_closes_or_reverses() {
    let directory = TestDirectory::new("workflow-position-triggers");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    for (workflow_id, trigger) in [
        ("on-open", WorkflowTrigger::PositionOpened),
        ("on-close", WorkflowTrigger::PositionClosed),
    ] {
        register(&service, position_watch(workflow_id, trigger));
        let armed = service.arm_workflow(workflow_id.into(), 1).expect("armed");
        assert!(armed.blockers.is_empty(), "{:?}", armed.blockers);
    }
    let firings = |service: &TradingService| {
        (
            workflow(service, "on-open").total_firings,
            workflow(service, "on-close").total_firings,
        )
    };
    let book = (9_975, 10_000);
    assert_eq!(firings(&service), (0, 0));
    trade(
        &service,
        &clock,
        &feed,
        ("open", OrderSide::Buy, 1),
        2,
        book,
    );
    assert_eq!(firings(&service), (1, 0), "opening fires the open trigger");
    trade(
        &service,
        &clock,
        &feed,
        ("close", OrderSide::Sell, 1),
        3,
        book,
    );
    assert_eq!(firings(&service), (1, 1), "closing fires the close trigger");
    trade(
        &service,
        &clock,
        &feed,
        ("reopen", OrderSide::Buy, 1),
        4,
        book,
    );
    assert_eq!(firings(&service), (2, 1));
    trade(
        &service,
        &clock,
        &feed,
        ("reverse", OrderSide::Sell, 2),
        5,
        book,
    );
    assert_eq!(
        firings(&service),
        (3, 2),
        "a reversal closes one side and opens the other"
    );
    assert_eq!(working_set(&service).positions[0].net_quantity.units(), -1);
    quote_now(&service, &clock, &feed, 6, (9_950, 9_975));
    assert_eq!(firings(&service), (3, 2), "a quote alone moves no position");
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn trade_checks_read_time_in_trade_open_profit_and_pullback() {
    let directory = TestDirectory::new("workflow-trade-checks");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let amount = |units| FixedPoint::try_new(units, 2).expect("amount");
    let (at_least, at_most) = (WorkflowCompare::AtLeast, WorkflowCompare::AtMost);
    let probes = [
        (
            "in-trade-at-most-1m",
            WorkflowCheck::TimeInTrade {
                compare: at_most,
                minutes: 1,
            },
            true,
        ),
        (
            "in-trade-at-least-1m",
            WorkflowCheck::TimeInTrade {
                compare: at_least,
                minutes: 1,
            },
            false,
        ),
        (
            "up-2-ticks",
            WorkflowCheck::OpenProfitTicks {
                compare: at_least,
                ticks: 2,
            },
            true,
        ),
        (
            "up-3-ticks",
            WorkflowCheck::OpenProfitTicks {
                compare: at_least,
                ticks: 3,
            },
            false,
        ),
        (
            "down-1-tick",
            WorkflowCheck::OpenProfitTicks {
                compare: at_most,
                ticks: -1,
            },
            false,
        ),
        (
            "up-1-dollar",
            WorkflowCheck::OpenProfit {
                compare: at_least,
                amount: amount(100),
            },
            true,
        ),
        (
            "not-up",
            WorkflowCheck::OpenProfit {
                compare: at_most,
                amount: amount(0),
            },
            false,
        ),
        ("pulled-2-ticks", WorkflowCheck::Pullback { ticks: 2 }, true),
        (
            "pulled-3-ticks",
            WorkflowCheck::Pullback { ticks: 3 },
            false,
        ),
    ];
    for (workflow_id, check, _) in probes {
        register(&service, probe(workflow_id, check));
        service.arm_workflow(workflow_id.into(), 1).expect("armed");
    }
    // Long one from 100.00; the bid runs to 101.00, then comes back to 100.50: two ticks
    // up from the entry and two back from the best exit.
    trade(
        &service,
        &clock,
        &feed,
        ("open", OrderSide::Buy, 1),
        2,
        (9_975, 10_000),
    );
    quote_now(&service, &clock, &feed, 3, (10_100, 10_125));
    quote_now(&service, &clock, &feed, 4, (10_050, 10_075));
    // The fixture clock started at 00:16:40 UTC; the probes fire at 00:17.
    clock.advance(Duration::from_secs(20));
    service.status().expect("owner runs the due timer");
    for (workflow_id, _, expected) in probes {
        assert_eq!(
            branch_taken(&service, workflow_id),
            expected,
            "{workflow_id}"
        );
    }
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn the_open_time_survives_a_restart_and_an_unknown_one_blocks() {
    let directory = TestDirectory::new("workflow-open-time");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let held = WorkflowCheck::TimeInTrade {
        compare: WorkflowCompare::AtLeast,
        minutes: 1,
    };
    register(&service, probe("held-a-minute", held));
    register(
        &service,
        position_watch("on-open", WorkflowTrigger::PositionOpened),
    );
    for workflow_id in ["held-a-minute", "on-open"] {
        service.arm_workflow(workflow_id.into(), 1).expect("armed");
    }
    trade(
        &service,
        &clock,
        &feed,
        ("open", OrderSide::Buy, 1),
        2,
        (9_975, 10_000),
    );
    assert_eq!(workflow(&service, "on-open").total_firings, 1);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");

    // A restart only records the open position; the open time is durable.
    let (restarted, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument, market_feed::route());
    quote_now(&restarted, &clock, &feed, 3, (9_975, 10_000));
    assert_eq!(workflow(&restarted, "on-open").total_firings, 1);
    clock.advance(Duration::from_secs(25));
    restarted.status().expect("owner runs the due timer");
    assert!(
        !branch_taken(&restarted, "held-a-minute"),
        "open for 20 seconds"
    );
    clock.advance(Duration::from_secs(60));
    restarted.status().expect("owner runs the due timer");
    assert!(
        branch_taken(&restarted, "held-a-minute"),
        "open for 80 seconds"
    );
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");

    // A position whose opening fill Aeris never accounted has no open time to read.
    rusqlite::Connection::open(directory.0.join("trading.sqlite3"))
        .expect("database opens")
        .execute("UPDATE trade_cycles SET opened_unix_nanos = NULL", [])
        .expect("open time cleared");
    let (restarted, _feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument, market_feed::route());
    clock.advance(Duration::from_secs(60));
    restarted.status().expect("owner runs the due timer");
    assert_eq!(
        workflow(&restarted, "held-a-minute").recent_firings[0].outcome,
        WorkflowFiringOutcome::Blocked(vec![WorkflowBlocker::PositionUnknown])
    );
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn an_open_profit_amount_must_be_at_the_account_currency_scale() {
    let directory = TestDirectory::new("workflow-amount-scale");
    let instrument = instrument();
    let (service, _clock, _feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let check = WorkflowCheck::OpenProfit {
        compare: WorkflowCompare::AtLeast,
        amount: FixedPoint::try_new(100, 3).expect("amount"),
    };
    assert_eq!(
        service.register_workflow(probe("off-scale", check), WorkflowLayout::default()),
        Err(WorkflowError::Invalid(WorkflowInvalid::Amount))
    );
    let day = WorkflowCheck::DayProfit {
        compare: WorkflowCompare::AtMost,
        amount: FixedPoint::try_new(-100, 0).expect("amount"),
    };
    assert_eq!(
        service.register_workflow(probe("day-off-scale", day), WorkflowLayout::default()),
        Err(WorkflowError::Invalid(WorkflowInvalid::Amount))
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn deleting_a_practice_account_removes_its_workflows() {
    let directory = TestDirectory::new("workflow-account-delete");
    let instrument = instrument();
    let (service, clock, _feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let practice = service
        .create_practice_account(CreatePracticeAccount {
            display_name: "Workflow".into(),
            starting_equity: FixedPoint::try_new(1_000_000, 2).expect("equity"),
        })
        .expect("practice account");
    let mut definition = breakout("practice-breakout", 10_050);
    definition.account_id = practice.id.clone();
    register(&service, definition);
    register(&service, breakout("kept", 10_050));
    service
        .delete_practice_account(practice.id)
        .expect("account deleted");
    let ids = |service: &TradingService| {
        working_set(service)
            .workflows
            .into_iter()
            .map(|view| view.definition.workflow_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&service), vec!["kept".to_string()]);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
    let (restarted, _feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument, market_feed::route());
    assert_eq!(ids(&restarted), vec!["kept".to_string()]);
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

/// A second fixture symbol, quoted near 80.00.
fn crude() -> TradingInstrument {
    let mut crude = instrument();
    crude.instrument_id =
        InstrumentId::try_new("instrument:fixture:CME:CLZ6").expect("crude instrument id");
    crude
}

fn crude_route() -> aeris_trading::market::MarketRoute {
    aeris_trading::market::MarketRoute {
        instrument_id: crude().instrument_id,
        provider_symbol: "CLZ6".into(),
        display_symbol: "CLZ6".into(),
        ..market_feed::route()
    }
}

/// The quoted fixture with the second symbol quoted too; both carry ticket demand, as open
/// charts would hold it.
fn two_symbol_fixture(
    directory: &TestDirectory,
) -> (
    TradingService,
    Arc<crate::clock::ManualClock>,
    Arc<crate::core::market::feed::MarketFeedInbox>,
) {
    let (service, clock, feed) = quoted_fixture(
        directory,
        &instrument(),
        market_feed::route(),
        9_975,
        10_000,
    );
    service
        .register_instrument(crude())
        .expect("crude registers");
    service
        .set_ticket_interest(vec![market_feed::route(), crude_route()])
        .expect("ticket interest");
    bootstrap_live_feed(&feed, &crude(), crude_route());
    push_live_quote(&feed, &crude(), 1, 7_975, 8_000, 2);
    service.status().expect("owner drains the crude feed");
    (service, clock, feed)
}

/// A resting buy limit in `instrument` far below its quote.
fn resting_limit_in(instrument: &TradingInstrument, client_id: &str, time: i64) -> PlaceOrder {
    PlaceOrder {
        instrument_id: instrument.instrument_id.clone(),
        limit_price: Some(price(6_000)),
        ..resting_limit(client_id, time)
    }
}

fn net_quantity(service: &TradingService, instrument: &TradingInstrument) -> i64 {
    working_set(service)
        .positions
        .iter()
        .find(|position| {
            position.account_id == account() && position.instrument_id == instrument.instrument_id
        })
        .map_or(0, |position| position.net_quantity.units())
}

#[test]
fn a_workflow_on_any_symbol_cancels_and_flattens_every_symbol() {
    let directory = TestDirectory::new("workflow-any-symbol-flat");
    let (service, clock, feed) = two_symbol_fixture(&directory);
    for (instrument, client_id) in [(instrument(), "rest-es"), (crude(), "rest-cl")] {
        service
            .place_order(
                resting_limit_in(&instrument, client_id, clock.wall_unix_nanos()),
                false,
            )
            .expect("resting order");
    }
    trade_in(
        &service,
        &clock,
        &feed,
        &instrument(),
        ("es-long", OrderSide::Buy, 1),
        2,
        (9_975, 10_000),
    );
    trade_in(
        &service,
        &clock,
        &feed,
        &crude(),
        ("cl-short", OrderSide::Sell, 2),
        2,
        (7_975, 8_000),
    );
    assert_eq!(
        (
            net_quantity(&service, &instrument()),
            net_quantity(&service, &crude())
        ),
        (1, -2)
    );

    // Every minute: cancel the working orders, then flatten, on whatever symbol they are.
    let mut definition = position_watch("flat-everything", WorkflowTrigger::Every { minutes: 1 });
    definition.scope = WorkflowScope::AnySymbol;
    definition.nodes = vec![
        WorkflowNode {
            id: THEN_NODE,
            step: WorkflowStep::Action {
                action: WorkflowAction::CancelOrders,
                next: Some(ELSE_NODE),
            },
        },
        action_node(ELSE_NODE, WorkflowAction::Flatten),
    ];
    register(&service, definition);
    let armed = service
        .arm_workflow("flat-everything".into(), 1)
        .expect("armed");
    assert!(armed.blockers.is_empty(), "{:?}", armed.blockers);

    // The fixture clock is 00:16:40 UTC; the next minute mark is 00:17.
    clock.advance(Duration::from_secs(20));
    service.status().expect("owner runs the due timer");
    assert_eq!(
        executed(&workflow(&service, "flat-everything")),
        vec![
            WorkflowStepResult {
                node: THEN_NODE,
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Cancelled { orders: 2 }),
            },
            WorkflowStepResult {
                node: ELSE_NODE,
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Flattened),
            },
        ]
    );
    assert_eq!(
        (
            net_quantity(&service, &instrument()),
            net_quantity(&service, &crude())
        ),
        (0, 0)
    );
    assert_eq!(working_set(&service).orders, []);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_position_trigger_on_any_symbol_acts_on_the_symbol_that_moved() {
    let directory = TestDirectory::new("workflow-any-symbol-moved");
    let (service, clock, feed) = two_symbol_fixture(&directory);
    for (instrument, client_id) in [(instrument(), "rest-es"), (crude(), "rest-cl")] {
        service
            .place_order(
                resting_limit_in(&instrument, client_id, clock.wall_unix_nanos()),
                false,
            )
            .expect("resting order");
    }
    let mut definition = position_watch("cancel-on-open", WorkflowTrigger::PositionOpened);
    definition.scope = WorkflowScope::AnySymbol;
    register(&service, definition);
    service
        .arm_workflow("cancel-on-open".into(), 1)
        .expect("armed");

    trade_in(
        &service,
        &clock,
        &feed,
        &crude(),
        ("cl-short", OrderSide::Sell, 1),
        2,
        (7_975, 8_000),
    );
    let view = workflow(&service, "cancel-on-open");
    assert_eq!(view.total_firings, 1, "opening crude fires the trigger");
    assert_eq!(
        executed(&view),
        vec![WorkflowStepResult {
            node: THEN_NODE,
            outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Cancelled { orders: 1 }),
        }]
    );
    let orders = working_set(&service).orders;
    assert_eq!(orders.len(), 1, "only the symbol that opened is acted on");
    assert_eq!(orders[0].instrument_id, instrument().instrument_id);

    trade_in(
        &service,
        &clock,
        &feed,
        &instrument(),
        ("es-long", OrderSide::Buy, 1),
        3,
        (9_975, 10_000),
    );
    assert_eq!(workflow(&service, "cancel-on-open").total_firings, 2);
    assert_eq!(working_set(&service).orders, []);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn an_if_on_any_symbol_narrows_the_run_to_the_positions_that_pass() {
    let directory = TestDirectory::new("workflow-any-symbol-narrow");
    let (service, clock, feed) = two_symbol_fixture(&directory);
    trade_in(
        &service,
        &clock,
        &feed,
        &instrument(),
        ("es-long", OrderSide::Buy, 1),
        2,
        (9_975, 10_000),
    );
    trade_in(
        &service,
        &clock,
        &feed,
        &crude(),
        ("cl-short", OrderSide::Sell, 1),
        2,
        (7_975, 8_000),
    );
    // Every minute: if long, flatten; the short crude position does not pass.
    let mut definition = probe(
        "flatten-longs",
        WorkflowCheck::Position(PositionRequirement::Long),
    );
    definition.scope = WorkflowScope::AnySymbol;
    definition.nodes[1] = action_node(THEN_NODE, WorkflowAction::Flatten);
    register(&service, definition);
    service
        .arm_workflow("flatten-longs".into(), 1)
        .expect("armed");

    clock.advance(Duration::from_secs(20));
    service.status().expect("owner runs the due timer");
    let before = workflow(&service, "flatten-longs");
    assert_eq!(
        executed(&before),
        vec![
            WorkflowStepResult {
                node: IF_NODE,
                outcome: WorkflowStepOutcome::Branch(true),
            },
            WorkflowStepResult {
                node: THEN_NODE,
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Flattened),
            },
        ]
    );
    assert_eq!(
        (
            net_quantity(&service, &instrument()),
            net_quantity(&service, &crude())
        ),
        (0, -1)
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");

    // A workflow on any symbol is stored without an instrument and reopens unchanged.
    let (restarted, _feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument(), market_feed::route());
    let after = workflow(&restarted, "flatten-longs");
    assert_eq!(after.definition.scope, WorkflowScope::AnySymbol);
    assert_eq!(after.definition, before.definition);
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

/// When `condition` becomes true, notify, every time.
fn condition_notice(workflow_id: &str, condition: WorkflowCondition) -> WorkflowDefinition {
    let mut definition = single(workflow_id, 10_050, WorkflowAction::Notify);
    definition.triggers = vec![WorkflowTrigger::ConditionMet(condition)];
    definition.limits.repeat = WorkflowRepeat::Always;
    definition
}

fn notified() -> Vec<WorkflowStepResult> {
    vec![WorkflowStepResult {
        node: THEN_NODE,
        outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Notified),
    }]
}

fn money(units: i64) -> FixedPoint {
    FixedPoint::try_new(units, 2).expect("amount")
}

#[test]
fn a_condition_trigger_fires_each_time_it_becomes_true() {
    let directory = TestDirectory::new("workflow-condition-edge");
    let instrument = instrument();
    let (service, _clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let above = WorkflowCondition::single(WorkflowCheck::Price {
        direction: PriceDirection::Above,
        level: price(10_050),
    });
    register(&service, condition_notice("bid-above", above));
    service.arm_workflow("bid-above".into(), 1).expect("armed");
    // Already true when armed: arming only records it.
    deliver_quote(&service, &feed, &instrument, 2, 10_075, 10_100);
    register(&service, condition_notice("armed-above", above));
    service
        .arm_workflow("armed-above".into(), 1)
        .expect("armed");
    deliver_quote(&service, &feed, &instrument, 3, 10_100, 10_125);
    let total = |workflow_id: &str| workflow(&service, workflow_id).total_firings;
    assert_eq!(
        (total("bid-above"), total("armed-above")),
        (1, 0),
        "false at arming then true fires; true at arming does not"
    );
    assert_eq!(executed(&workflow(&service, "bid-above")), notified());

    // Staying true fires nothing more; going false and true again fires once more.
    deliver_quote(&service, &feed, &instrument, 4, 10_125, 10_150);
    assert_eq!(total("bid-above"), 1);
    deliver_quote(&service, &feed, &instrument, 5, 10_000, 10_025);
    deliver_quote(&service, &feed, &instrument, 6, 10_050, 10_075);
    assert_eq!((total("bid-above"), total("armed-above")), (2, 1));
    let view = workflow(&service, "bid-above");
    assert_eq!(
        view.state,
        WorkflowState::Armed,
        "every time keeps it armed"
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_clock_condition_is_read_at_each_whole_minute() {
    let directory = TestDirectory::new("workflow-condition-clock");
    let instrument = instrument();
    let (service, clock, _feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    // The fixture clock is 00:16:40 UTC; the window opens at 00:17.
    let window = WorkflowCondition::single(WorkflowCheck::Between {
        zone: WorkflowTimeZone::Utc,
        start_minute: 17,
        end_minute: 18,
    });
    register(&service, condition_notice("in-window", window));
    service.arm_workflow("in-window".into(), 1).expect("armed");
    let timer_calls = || {
        service
            .request(Command::OwnerDiagnostics)
            .expect("owner diagnostics")
            .1
    };
    let before = timer_calls();
    clock.advance(Duration::from_secs(25));
    service.status().expect("owner runs the due timer");
    assert_eq!(timer_calls(), before + 1, "the minute mark is a deadline");
    let view = workflow(&service, "in-window");
    assert_eq!(view.total_firings, 1);
    assert_eq!(executed(&view), notified());
    clock.advance(Duration::from_secs(60));
    service.status().expect("owner runs the due timer");
    assert_eq!(workflow(&service, "in-window").total_firings, 1);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn the_day_profit_adds_realized_and_open_pnl_and_a_lock_can_flatten() {
    let directory = TestDirectory::new("workflow-day-profit");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let day = |compare, units| {
        WorkflowCondition::single(WorkflowCheck::DayProfit {
            compare,
            amount: money(units),
        })
    };
    register(
        &service,
        condition_notice("up-25", day(WorkflowCompare::AtLeast, 2_500)),
    );
    // Down 25.00 on the day: lock the account, then flatten every symbol.
    let mut stop = condition_notice("down-25", day(WorkflowCompare::AtMost, -2_500));
    stop.scope = WorkflowScope::AnySymbol;
    stop.limits.repeat = WorkflowRepeat::Once;
    stop.nodes = vec![
        WorkflowNode {
            id: THEN_NODE,
            step: WorkflowStep::Action {
                action: WorkflowAction::LockAccount,
                next: Some(ELSE_NODE),
            },
        },
        action_node(ELSE_NODE, WorkflowAction::Flatten),
    ];
    register(&service, stop);
    for workflow_id in ["up-25", "down-25"] {
        let armed = service.arm_workflow(workflow_id.into(), 1).expect("armed");
        assert!(armed.blockers.is_empty(), "{:?}", armed.blockers);
    }
    let total = |workflow_id: &str| workflow(&service, workflow_id).total_firings;

    // Long one from 100.00 at 50.00 a point: 12.50, then 25.00 open.
    trade(
        &service,
        &clock,
        &feed,
        ("open", OrderSide::Buy, 1),
        2,
        (10_025, 10_050),
    );
    assert_eq!(total("up-25"), 0);
    quote_now(&service, &clock, &feed, 3, (10_050, 10_075));
    assert_eq!(total("up-25"), 1);
    assert_eq!(executed(&workflow(&service, "up-25")), notified());
    // Closed at 100.75: 37.50 realized keeps the day up without a position.
    quote_now(&service, &clock, &feed, 4, (10_075, 10_100));
    trade(
        &service,
        &clock,
        &feed,
        ("close", OrderSide::Sell, 1),
        5,
        (10_000, 10_025),
    );
    assert_eq!(total("up-25"), 1, "still true, so no new firing");

    // Long again from 100.25: 50.00 open loss is 12.50 down on the day, 75.00 is 37.50.
    trade(
        &service,
        &clock,
        &feed,
        ("again", OrderSide::Buy, 1),
        6,
        (9_925, 9_950),
    );
    assert_eq!(total("down-25"), 0, "the realized profit counts");
    quote_now(&service, &clock, &feed, 7, (9_875, 9_900));
    let view = workflow(&service, "down-25");
    assert_eq!(view.state, WorkflowState::Completed);
    assert_eq!(
        executed(&view),
        vec![
            WorkflowStepResult {
                node: THEN_NODE,
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Locked),
            },
            WorkflowStepResult {
                node: ELSE_NODE,
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Flattened),
            },
        ]
    );
    quote_now(&service, &clock, &feed, 8, (9_875, 9_900));
    assert_eq!(net_quantity(&service, &instrument), 0);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn a_condition_on_any_symbol_acts_on_the_positions_that_became_true() {
    let directory = TestDirectory::new("workflow-condition-any-symbol");
    let (service, clock, feed) = two_symbol_fixture(&directory);
    // Up two ticks in any symbol: flatten that symbol.
    let mut definition = condition_notice(
        "take-two-ticks",
        WorkflowCondition::single(WorkflowCheck::OpenProfitTicks {
            compare: WorkflowCompare::AtLeast,
            ticks: 2,
        }),
    );
    definition.scope = WorkflowScope::AnySymbol;
    definition.nodes = vec![action_node(THEN_NODE, WorkflowAction::Flatten)];
    register(&service, definition);
    service
        .arm_workflow("take-two-ticks".into(), 1)
        .expect("armed");
    trade_in(
        &service,
        &clock,
        &feed,
        &instrument(),
        ("es-long", OrderSide::Buy, 1),
        2,
        (9_975, 10_000),
    );
    trade_in(
        &service,
        &clock,
        &feed,
        &crude(),
        ("cl-short", OrderSide::Sell, 2),
        2,
        (7_975, 8_000),
    );
    assert_eq!(workflow(&service, "take-two-ticks").total_firings, 0);

    deliver_quote(&service, &feed, &instrument(), 3, 10_050, 10_075);
    assert_eq!(workflow(&service, "take-two-ticks").total_firings, 1);
    assert_eq!(
        (
            net_quantity(&service, &instrument()),
            net_quantity(&service, &crude())
        ),
        (0, -2),
        "only the symbol that became true is flattened"
    );
    deliver_quote(&service, &feed, &crude(), 3, 7_900, 7_925);
    assert_eq!(workflow(&service, "take-two-ticks").total_firings, 2);
    assert_eq!(net_quantity(&service, &crude()), 0);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn condition_triggers_notices_and_every_time_survive_a_restart() {
    let directory = TestDirectory::new("workflow-condition-restart");
    let instrument = instrument();
    let (service, clock, feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let mut definition = condition_notice(
        "durable-condition",
        WorkflowCondition::single(WorkflowCheck::Price {
            direction: PriceDirection::Above,
            level: price(10_050),
        }),
    );
    definition.triggers.push(WorkflowTrigger::ConditionMet(
        WorkflowCondition::joined(
            WorkflowJoin::All,
            &[
                WorkflowCheck::DayProfit {
                    compare: WorkflowCompare::AtLeast,
                    amount: money(-10_000),
                },
                WorkflowCheck::Between {
                    zone: WorkflowTimeZone::Chicago,
                    start_minute: 0,
                    end_minute: 1_439,
                },
            ],
        )
        .expect("condition"),
    ));
    definition.nodes = vec![
        WorkflowNode {
            id: THEN_NODE,
            step: WorkflowStep::Action {
                action: WorkflowAction::Notify,
                next: Some(ELSE_NODE),
            },
        },
        action_node(ELSE_NODE, WorkflowAction::CancelOrders),
    ];
    register(&service, definition);
    service
        .arm_workflow("durable-condition".into(), 1)
        .expect("armed");
    deliver_quote(&service, &feed, &instrument, 2, 10_050, 10_075);
    let before = workflow(&service, "durable-condition");
    assert_eq!(before.total_firings, 1);
    assert_eq!(
        executed(&before),
        vec![
            WorkflowStepResult {
                node: THEN_NODE,
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Notified),
            },
            WorkflowStepResult {
                node: ELSE_NODE,
                outcome: WorkflowStepOutcome::Action(WorkflowActionResult::Cancelled { orders: 0 }),
            },
        ]
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");

    let (restarted, feed) =
        reopen_awaiting_quote(&directory, &clock, &instrument, market_feed::route());
    let after = workflow(&restarted, "durable-condition");
    assert_eq!(after.definition, before.definition);
    assert_eq!(after.definition.limits.repeat, WorkflowRepeat::Always);
    assert_eq!(after.state, WorkflowState::Armed);
    assert_eq!(after.recent_firings, before.recent_firings);
    // The first read after the restart only records that the condition holds.
    deliver_quote(&restarted, &feed, &instrument, 3, 10_050, 10_075);
    assert_eq!(workflow(&restarted, "durable-condition").total_firings, 1);
    deliver_quote(&restarted, &feed, &instrument, 4, 10_000, 10_025);
    deliver_quote(&restarted, &feed, &instrument, 5, 10_050, 10_075);
    assert_eq!(workflow(&restarted, "durable-condition").total_firings, 2);
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}
