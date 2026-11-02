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
        instrument_id: instrument().instrument_id,
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
    definition.instrument_id = eth.instrument_id.clone();
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
    definition.instrument_id = eth.instrument_id.clone();
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
                    ],
                )
                .expect("condition"),
                then: Some(THEN_NODE),
                otherwise: None,
            },
        },
        action_node(THEN_NODE, WorkflowAction::CancelOrders),
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
