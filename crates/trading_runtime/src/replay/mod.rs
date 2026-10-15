//! Frozen simulated-venue behavior, recorded before broker venue routing was introduced.
//! Do not update the expected ledger to accommodate a simulated-path refactor.

use super::*;
use std::fmt::Write;

/// Single-threaded replay of the production core against a real, private SQLite file.
struct ReplayHarness {
    core: Option<TradingCore>,
    configuration: TradingServiceConfig,
}

impl ReplayHarness {
    fn new(configuration: TradingServiceConfig) -> Result<Self, StoreError> {
        let core = TradingCore::open(
            &configuration.database_path,
            configuration.retention.validate()?,
            configuration.clock.clone(),
            configuration.execution,
            Weak::new(),
            Arc::new(crate::core::market::feed::MarketFeedInbox::new(
                mpsc::sync_channel(1).0,
                Arc::new(AtomicBool::new(false)),
            )),
        )?;
        Ok(Self {
            core: Some(core),
            configuration,
        })
    }

    fn restart(&mut self) -> Result<(), StoreError> {
        // Releasing the SQLite connection is part of the restart, not merely reloading state.
        drop(self.core.take());
        self.core = Self::new(self.configuration.clone())?.core;
        Ok(())
    }

    fn current(&self) -> &TradingCore {
        self.core.as_ref().expect("replay core is open")
    }

    fn crash_during_commit(&mut self, nth: usize) {
        assert!(nth > 0);
        self.current().store.crash_during_commit(nth);
    }

    fn command<T>(&mut self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (sender, receiver) = mpsc::sync_channel(REPLY_CAPACITY);
        assert!(
            self.core
                .as_mut()
                .expect("replay core is open")
                .handle(make(sender), self.configuration.clock.wall_unix_nanos())
        );
        receiver.recv().expect("core replied to replay command")
    }

    fn register_account(&mut self, account: TradingAccount) -> Result<(), String> {
        self.command(|reply| Command::RegisterAccount(account, reply))
    }

    fn register_instrument(&mut self, instrument: TradingInstrument) -> Result<(), String> {
        self.command(|reply| Command::RegisterInstrument(instrument, reply))
    }

    fn place_order(&mut self, order: PlaceOrder) -> Result<Order, String> {
        self.command(|reply| Command::Place(order, reply))
    }

    fn place_inline_bracket(
        &mut self,
        bracket: PlaceInlineBracket,
    ) -> Result<BracketPlacement, String> {
        self.command(|reply| Command::PlaceInlineBracket(bracket, reply))
    }

    fn observe_market(
        &mut self,
        observation: SimulatedMarketObservation,
    ) -> Result<Vec<Fill>, String> {
        self.command(|reply| Command::Observe(observation, reply))
    }

    fn modify_order(&mut self, order: ModifyOrder) -> Result<Order, String> {
        self.command(|reply| Command::Modify(order, reply))
    }

    fn cancel_order(&mut self, id: ClientOrderId) -> Result<Order, String> {
        self.command(|reply| Command::Cancel(id, reply))
    }

    fn flatten_account(
        &mut self,
        id: TradingAccountId,
        observation: SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        self.command(|reply| Command::Flatten(id, observation, reply))
    }

    fn reverse_position(
        &mut self,
        id: TradingAccountId,
        observation: SimulatedMarketObservation,
    ) -> Result<FlattenOutcome, String> {
        self.command(|reply| Command::Reverse(id, observation, reply))
    }

    fn kill_switch(
        &mut self,
        id: Option<TradingAccountId>,
        reason: String,
        at: i64,
    ) -> Result<usize, String> {
        self.command(|reply| Command::KillSwitch(id, reason, at, reply))
    }

    fn snapshot(&self) -> Result<TradingSnapshot, String> {
        self.current().snapshot()
    }

    fn transcript(&self) -> String {
        ledger(&self.snapshot().expect("replay snapshot"))
    }

    /// Fixed-seed, bounded command stream. Every mismatch includes the seed and step.
    fn run_seeded(&mut self, seed: u64, steps: usize) -> String {
        self.run_seeded_with_check(seed, steps, |_, _| true)
    }

    fn run_seeded_with_check(
        &mut self,
        seed: u64,
        steps: usize,
        check: impl Fn(usize, &TradingSnapshot) -> bool,
    ) -> String {
        assert!(steps <= 256, "replay seed={seed}: too many steps");
        let mut random = XorShift(seed);
        let mut output = String::new();
        for step in 0..steps {
            let value = random.next();
            let now = 10_000 + i64::try_from(step).expect("bounded replay steps");
            match value % 4 {
                0 => {
                    let mut order = market_order(
                        &format!("seed-{seed}-{step}"),
                        OrderSide::Buy,
                        step as u64 + 1,
                        now,
                    );
                    order.order_type = OrderType::Limit;
                    order.time_in_force = TimeInForce::GoodTillCancelled;
                    order.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
                    self.place_order(order).unwrap_or_else(|error| {
                        panic!("replay seed={seed} step={step} submit: {error}")
                    });
                }
                1 => {
                    let bid = if value & 8 == 0 { 10_000 } else { 8_975 };
                    self.observe_market(observation(bid, bid + 25, step as u64 + 1, now))
                        .unwrap_or_else(|error| {
                            panic!("replay seed={seed} step={step} quote: {error}")
                        });
                }
                2 => {
                    let open = self
                        .snapshot()
                        .expect("snapshot")
                        .orders
                        .into_iter()
                        .find(|order| order.status.is_open());
                    if let Some(order) = open {
                        self.cancel_order(order.client_order_id)
                            .unwrap_or_else(|error| {
                                panic!("replay seed={seed} step={step} cancel: {error}")
                            });
                    }
                }
                _ => {
                    let before = self.snapshot().expect("before restart");
                    self.restart().unwrap_or_else(|error| {
                        panic!("replay seed={seed} step={step} restart: {error}")
                    });
                    let recovered = self.snapshot().expect("after restart");
                    assert_eq!(
                        (
                            recovered.revision,
                            recovered.orders,
                            recovered.order_events,
                            recovered.fills
                        ),
                        (
                            before.revision,
                            before.orders,
                            before.order_events,
                            before.fills
                        ),
                        "replay seed={seed} step={step}: durable recovery (marks are memory-only)"
                    );
                }
            }
            let snapshot = self.snapshot().expect("snapshot");
            assert!(
                snapshot.orders.iter().all(|order| {
                    snapshot
                        .order_events
                        .iter()
                        .any(|event| event.order_id == order.id)
                }),
                "replay seed={seed} step={step}: orphan order"
            );
            assert!(
                check(step, &snapshot),
                "replay seed={seed} step={step}: property failed"
            );
            writeln!(
                output,
                "seed={seed} step={step} revision={}",
                snapshot.revision
            )
            .expect("write transcript");
        }
        output.push_str(&self.transcript());
        output
    }
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        // An all-zero seed is still a valid reproducible run.
        let mut value = self.0 | 1;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }
}

fn point(value: FixedPoint) -> String {
    format!("{}@{}", value.units(), value.scale())
}

fn optional_point(value: Option<FixedPoint>) -> String {
    value.map_or_else(|| "-".to_string(), point)
}

fn origin(value: &TradingProvenance) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        value.venue_id,
        value.provider_id,
        value.session_generation,
        value.source_sequence,
        value.observed_unix_nanos
    )
}

fn record(stage: &str, service: &ReplayHarness, trace: &mut String) {
    let snapshot = service.snapshot().expect("stage snapshot");
    let orders = snapshot
        .orders
        .iter()
        .map(|order| format!("{}:{}", order.id.as_str(), order.status.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let events = snapshot
        .order_events
        .iter()
        .map(|event| event.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let fills = snapshot
        .fills
        .iter()
        .map(|fill| fill.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let pnl = &snapshot.account_pnl[0];
    writeln!(
        trace,
        "{stage} revision={} orders=[{orders}] events=[{events}] fills=[{fills}] pnl={}/{}/{}",
        snapshot.revision,
        point(pnl.realized),
        optional_point(pnl.unrealized),
        optional_point(pnl.equity)
    )
    .expect("write stage");
}

fn ledger(snapshot: &TradingSnapshot) -> String {
    let mut result = String::new();
    writeln!(result, "revision={}", snapshot.revision).expect("write revision");
    for order in &snapshot.orders {
        writeln!(
            result,
            "order {} {} {} {} {} {} {} {} {} {} {} {}",
            order.id.as_str(),
            order.client_order_id.as_str(),
            order.side.as_str(),
            order.order_type.as_str(),
            order.time_in_force.as_str(),
            point(order.quantity),
            point(order.filled_quantity),
            optional_point(order.limit_price),
            optional_point(order.stop_price),
            order.status.as_str(),
            order.submitted_unix_nanos,
            origin(&order.provenance)
        )
        .expect("write order");
    }
    for event in &snapshot.order_events {
        writeln!(
            result,
            "event {} {} {} {} {} {:?} {}",
            event.id.as_str(),
            event.order_id.as_str(),
            event.sequence,
            event.kind.as_str(),
            event.event_unix_nanos,
            event.detail,
            origin(&event.provenance)
        )
        .expect("write event");
    }
    for fill in &snapshot.fills {
        writeln!(
            result,
            "fill {} {} {} {} {} {} {} {} {}",
            fill.id.as_str(),
            fill.order_id.as_str(),
            fill.account_id.as_str(),
            fill.instrument_id.as_str(),
            fill.side.as_str(),
            point(fill.price),
            point(fill.quantity),
            fill.execution_unix_nanos,
            origin(&fill.provenance)
        )
        .expect("write fill");
    }
    for position in &snapshot.positions {
        writeln!(
            result,
            "position {} {} {} {} {} {} {}",
            position.account_id.as_str(),
            position.instrument_id.as_str(),
            point(position.net_quantity),
            optional_point(position.average_entry_price),
            point(position.realized_pnl),
            point(position.unrealized_pnl),
            position.last_fill_unix_nanos
        )
        .expect("write position");
    }
    for pnl in &snapshot.account_pnl {
        writeln!(
            result,
            "pnl {} {} {} {} {}",
            pnl.account_id.as_str(),
            pnl.currency,
            point(pnl.realized),
            optional_point(pnl.unrealized),
            optional_point(pnl.equity)
        )
        .expect("write pnl");
    }
    for (id, value) in &snapshot.fill_realized_pnl {
        writeln!(result, "fill_pnl {} {}", id.as_str(), point(*value)).expect("write fill pnl");
    }
    for (id, value) in &snapshot.completed_trade_pnl {
        writeln!(result, "trade_pnl {} {}", id.as_str(), point(*value))
            .expect("write completed pnl");
    }
    writeln!(result, "managed_brackets={:?}", snapshot.managed_brackets).expect("write brackets");
    writeln!(result, "risk_locks={:?}", snapshot.risk_locks).expect("write locks");
    result
}

fn start_golden_service(
    directory: &TestDirectory,
    clock: std::sync::Arc<crate::clock::ManualClock>,
) -> ReplayHarness {
    let mut configuration = config(directory);
    configuration.clock = clock;
    configuration.retention.maximum_fills = 64;
    configuration.retention.maximum_order_events = 64;
    let mut service = ReplayHarness::new(configuration).expect("core starts");
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    service
        .register_account(TradingAccount {
            id: account.clone(),
            display_name: "SIM • Test".to_string(),
            environment: AccountEnvironment::Simulated,
            venue_id: "aeris-sim".to_string(),
            broker_ref: None,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: Some(FixedPoint::try_new(5_000_000, 2).expect("equity")),
        })
        .expect("account registers");
    service
        .register_instrument(instrument())
        .expect("instrument registers");
    service
}

#[test]
fn replay_multi_effect_quote_failed_commit_keeps_memory_and_disk_unchanged() {
    let directory = TestDirectory::new("multi-effect-quote-failure");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    harness
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order("atomic-bracket", OrderSide::Buy, 1, 1_000),
            template: BracketStrategyTemplate {
                template_id: "atomic-template".into(),
                revision: 1,
                name: "Atomic stop and target".into(),
                stop_offset_ticks: 8,
                targets: vec![BracketTarget {
                    offset_ticks: 8,
                    quantity_percent: 100,
                }],
                trailing_stop: None,
                break_even: None,
                enabled: true,
            },
        })
        .expect("entry accepted");
    harness
        .place_order(market_order("atomic-second", OrderSide::Buy, 2, 1_000))
        .expect("second entry accepted");
    let before = harness.snapshot().expect("before");
    let touch = observation(10_000, 10_025, 3, 2_000);
    harness.crash_during_commit(1);
    assert!(harness.observe_market(touch.clone()).is_err());
    assert_eq!(harness.snapshot().expect("failed memory"), before);
    harness.restart().expect("reopen after failed quote");
    assert_eq!(harness.snapshot().expect("failed disk"), before);
    harness.crash_during_commit(2);
    assert_eq!(harness.observe_market(touch).expect("replay").len(), 2);
    let committed = harness.snapshot().expect("committed");
    assert_eq!(committed.fills.len(), 2);
    assert_eq!(
        committed.managed_brackets[0].status,
        ManagedBracketStatus::Active
    );
    harness.restart().expect("restart after committed quote");
    assert_eq!(harness.snapshot().expect("recovered"), committed);
}

#[test]
fn replay_failed_stop_fill_keeps_target_and_bracket_working_until_replay() {
    let directory = TestDirectory::new("quote-oco-failure");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    harness
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order("oco-entry", OrderSide::Buy, 1, 1_000),
            template: BracketStrategyTemplate {
                template_id: "oco-template".into(),
                revision: 1,
                name: "Atomic OCO".into(),
                stop_offset_ticks: 8,
                targets: vec![BracketTarget {
                    offset_ticks: 8,
                    quantity_percent: 100,
                }],
                trailing_stop: None,
                break_even: None,
                enabled: true,
            },
        })
        .expect("bracket placed");
    harness
        .observe_market(observation(10_000, 10_025, 2, 2_000))
        .expect("entry fills and arms");
    let before = harness.snapshot().expect("before stop fill");
    let touch = observation(9_800, 9_825, 3, 6_000);
    harness.crash_during_commit(1);
    assert!(harness.observe_market(touch.clone()).is_err());
    assert_eq!(harness.snapshot().expect("failed memory"), before);
    harness.restart().expect("restart after rollback");
    assert_eq!(harness.snapshot().expect("failed disk"), before);
    harness.crash_during_commit(2);
    assert_eq!(harness.observe_market(touch).expect("stop replay").len(), 1);
    let committed = harness.snapshot().expect("after stop");
    assert_eq!(
        committed.managed_brackets[0].status,
        ManagedBracketStatus::Completed
    );
    assert_eq!(committed.positions[0].net_quantity.units(), 0);
    assert!(
        committed
            .orders
            .iter()
            .any(|order| order.status == OrderStatus::Cancelled)
    );
    harness.restart().expect("restart after stop");
    assert_eq!(harness.snapshot().expect("durable OCO"), committed);
}

fn script_place_touch_modify_cancel_flatten(
    service: &mut ReplayHarness,
    account: &TradingAccountId,
    trace: &mut String,
) {
    let mut limit = market_order("touch-limit", OrderSide::Buy, 1, 1_000);
    limit.order_type = OrderType::Limit;
    limit.time_in_force = TimeInForce::GoodTillCancelled;
    limit.limit_price = Some(FixedPoint::try_new(10_000, 2).expect("limit price"));
    service.place_order(limit).expect("limit accepted");
    record("place", service, trace);
    assert_eq!(
        service
            .observe_market(observation(10_000, 10_025, 2, 2_000))
            .expect("above limit"),
        Vec::<Fill>::new()
    );
    record("no touch", service, trace);
    service
        .observe_market(observation(9_975, 10_000, 3, 3_000))
        .expect("limit touched");
    record("touch fill", service, trace);

    let mut resting = market_order("modify-cancel", OrderSide::Sell, 4, 4_000);
    resting.order_type = OrderType::Limit;
    resting.time_in_force = TimeInForce::GoodTillCancelled;
    resting.limit_price = Some(FixedPoint::try_new(10_500, 2).expect("resting price"));
    service.place_order(resting).expect("resting order");
    service
        .modify_order(ModifyOrder {
            client_order_id: ClientOrderId::try_new("modify-cancel").expect("client id"),
            time_in_force: TimeInForce::GoodTillCancelled,
            limit_price: Some(FixedPoint::try_new(10_400, 2).expect("modified price")),
            stop_price: None,
            modified_unix_nanos: 5_000,
            provenance: provenance(5, 5_000),
        })
        .expect("modify resting order");
    record("modify", service, trace);
    service
        .cancel_order(ClientOrderId::try_new("modify-cancel").expect("client id"))
        .expect("cancel resting order");
    record("cancel", service, trace);
    service
        .flatten_account(account.clone(), observation(10_100, 10_125, 6, 6_000))
        .expect("flatten first position");
    record("flatten", service, trace);
}

fn simulated_place_touch_modify_cancel_bracket_flatten_reverse_kill_scenario() -> String {
    let directory = TestDirectory::new("simulated-golden");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut service = start_golden_service(&directory, clock.clone());
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let mut trace = String::new();
    script_place_touch_modify_cancel_flatten(&mut service, &account, &mut trace);

    let template = BracketStrategyTemplate {
        template_id: "golden-bracket".to_string(),
        revision: 1,
        name: "Golden stop and target".to_string(),
        stop_offset_ticks: 8,
        targets: vec![BracketTarget {
            offset_ticks: 8,
            quantity_percent: 100,
        }],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    };
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order("bracket-entry", OrderSide::Buy, 7, 7_000),
            template,
        })
        .expect("bracket accepted");
    service
        .observe_market(observation(10_000, 10_025, 8, 8_000))
        .expect("bracket entry fills");
    record("bracket active", &service, &mut trace);
    clock.set_wall_unix_nanos(8_000);
    service
        .flatten_account(account.clone(), observation(10_100, 10_125, 9, 9_000))
        .expect("flatten bracket position");
    record("bracket flatten", &service, &mut trace);

    service
        .place_order(market_order("reverse-entry", OrderSide::Buy, 10, 10_000))
        .expect("reverse entry accepted");
    service
        .observe_market(observation(10_000, 10_025, 11, 11_000))
        .expect("reverse entry fills");
    service
        .reverse_position(account.clone(), observation(10_100, 10_125, 12, 12_000))
        .expect("reverse position");
    record("reverse", &service, &mut trace);

    let mut pending = market_order("kill-pending", OrderSide::Buy, 13, 13_000);
    pending.order_type = OrderType::Limit;
    pending.time_in_force = TimeInForce::GoodTillCancelled;
    pending.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("pending price"));
    service.place_order(pending).expect("pending order");
    clock.set_wall_unix_nanos(13_000);
    assert_eq!(
        service
            .kill_switch(Some(account), "golden kill".to_string(), 14_000)
            .expect("kill switch"),
        1
    );
    record("kill switch", &service, &mut trace);

    let actual = format!(
        "{trace}\n{}",
        ledger(&service.snapshot().expect("final snapshot"))
    );
    actual
}

#[test]
fn simulated_place_touch_modify_cancel_bracket_flatten_reverse_kill_golden() {
    let first = simulated_place_touch_modify_cancel_bracket_flatten_reverse_kill_scenario();
    assert_eq!(
        first,
        simulated_place_touch_modify_cancel_bracket_flatten_reverse_kill_scenario()
    );
    assert_eq!(
        first.trim_end(),
        include_str!("../tests/simulated_golden.txt")
            .replace("\r\n", "\n")
            .trim_end()
    );
}

fn simulated_bracket_stop_touch_cancels_target_scenario() -> String {
    let directory = TestDirectory::new("simulated-stop-golden");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(2_000));
    let mut service = start_golden_service(&directory, clock.clone());
    let mut trace = String::new();
    service
        .place_inline_bracket(PlaceInlineBracket {
            entry: market_order("stop-entry", OrderSide::Buy, 1, 1_000),
            template: BracketStrategyTemplate {
                template_id: "stop-golden".to_string(),
                revision: 1,
                name: "Stop touch fixture".to_string(),
                stop_offset_ticks: 8,
                targets: vec![BracketTarget {
                    offset_ticks: 8,
                    quantity_percent: 100,
                }],
                trailing_stop: None,
                break_even: None,
                enabled: true,
            },
        })
        .expect("bracket accepted");
    record("bracket placed", &service, &mut trace);
    service
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("entry touch");
    record("entry touched", &service, &mut trace);
    clock.set_wall_unix_nanos(3_000);
    service
        .observe_market(observation(9_750, 9_775, 3, 3_000))
        .expect("stop touch");
    record("stop touched", &service, &mut trace);
    let actual = format!(
        "{trace}\n{}",
        ledger(&service.snapshot().expect("final snapshot"))
    );
    actual
}

#[test]
fn simulated_bracket_stop_touch_cancels_target_golden() {
    let first = simulated_bracket_stop_touch_cancels_target_scenario();
    assert_eq!(
        first,
        simulated_bracket_stop_touch_cancels_target_scenario()
    );
    assert_eq!(
        first.trim_end(),
        include_str!("../tests/simulated_stop_golden.txt")
            .replace("\r\n", "\n")
            .trim_end()
    );
}

#[test]
fn replay_restart_preserves_committed_transcript() {
    let directory = TestDirectory::new("replay-restart");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let mut pending = market_order("restart-order", OrderSide::Buy, 1, 1_000);
    pending.order_type = OrderType::Limit;
    pending.time_in_force = TimeInForce::GoodTillCancelled;
    pending.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    harness.place_order(pending).expect("place");
    let before = harness.transcript();
    harness.restart().expect("reopen same sqlite file");
    assert_eq!(harness.transcript(), before);
}

#[test]
fn replay_failed_commit_keeps_memory_and_disk_at_last_commit() {
    let directory = TestDirectory::new("replay-commit");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let before = harness.transcript();
    harness.crash_during_commit(1);
    let mut pending = market_order("failed-order", OrderSide::Buy, 1, 1_000);
    pending.order_type = OrderType::Limit;
    pending.time_in_force = TimeInForce::GoodTillCancelled;
    pending.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    assert!(harness.place_order(pending).is_err());
    assert_eq!(harness.transcript(), before, "failed commit changed memory");
    harness.restart().expect("reopen after failed commit");
    assert_eq!(harness.transcript(), before, "failed commit changed disk");
}

#[test]
fn replay_failed_accept_cancel_and_amend_keep_publications_and_replay_once() {
    for operation in ["accept", "cancel", "amend"] {
        let directory = TestDirectory::new("command-commit");
        let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
        let mut harness = start_golden_service(&directory, clock);
        let mut pending = market_order("command-order", OrderSide::Buy, 1, 1_000);
        pending.order_type = OrderType::Limit;
        pending.time_in_force = TimeInForce::GoodTillCancelled;
        pending.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
        if operation != "accept" {
            harness
                .place_order(pending.clone())
                .expect("initial placement");
        }
        let before = harness.snapshot().expect("before");
        let apply = |harness: &mut ReplayHarness| -> Result<(), String> {
            match operation {
                "accept" => harness.place_order(pending.clone()).map(|_| ()),
                "cancel" => harness
                    .cancel_order(ClientOrderId::try_new("command-order").expect("id"))
                    .map(|_| ()),
                _ => harness
                    .modify_order(ModifyOrder {
                        client_order_id: ClientOrderId::try_new("command-order").expect("id"),
                        time_in_force: TimeInForce::GoodTillCancelled,
                        limit_price: Some(FixedPoint::try_new(8_900, 2).expect("price")),
                        stop_price: None,
                        modified_unix_nanos: 5_000,
                        provenance: provenance(3, 5_000),
                    })
                    .map(|_| ()),
            }
        };
        harness.crash_during_commit(1);
        assert!(apply(&mut harness).is_err(), "{operation}: failed commit");
        assert_eq!(
            harness.snapshot().expect("memory"),
            before,
            "{operation}: memory"
        );
        harness.restart().expect("reopen");
        assert_eq!(
            harness.snapshot().expect("disk"),
            before,
            "{operation}: disk"
        );
        apply(&mut harness).expect("replay once");
        let committed = harness.snapshot().expect("committed");
        assert_ne!(committed, before);
        harness.restart().expect("reopen after replay");
        assert_eq!(harness.snapshot().expect("recovered"), committed);
    }
}

#[test]
fn replay_failed_copied_accept_has_no_partial_orders_or_dispatches() {
    let directory = TestDirectory::new("copied-commit");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let target = TradingAccountId::try_new("aeris-sim-copy").expect("target");
    harness
        .register_account(TradingAccount {
            id: target.clone(),
            display_name: "SIM Copy".into(),
            environment: AccountEnvironment::Simulated,
            venue_id: "aeris-sim".into(),
            broker_ref: None,
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("target registered");
    harness
        .command(|reply| {
            Command::RegisterTradeCopier(
                TradeCopierConfig {
                    source_account_id: TradingAccountId::try_new("aeris-sim-1").expect("source"),
                    revision: 1,
                    enabled: true,
                    targets: vec![TradeCopierTarget {
                        account_id: target,
                        quantity_multiplier: FixedPoint::try_new(1, 0).expect("quantity"),
                        enabled: true,
                    }],
                },
                reply,
            )
        })
        .expect("copier registered");
    let before = harness.snapshot().expect("before");
    harness.crash_during_commit(1);
    let order = market_order("copied-failure", OrderSide::Buy, 1, 1_000);
    assert!(harness.place_order(order.clone()).is_err());
    assert_eq!(harness.snapshot().expect("memory"), before);
    harness.restart().expect("restart");
    assert_eq!(harness.snapshot().expect("disk"), before);
    harness.crash_during_commit(2);
    harness.place_order(order).expect("replay");
    assert_eq!(harness.snapshot().expect("accepted").orders.len(), 2);
    let committed = harness.snapshot().expect("committed");
    harness.restart().expect("restart after successful copy");
    let mut durable = committed.clone();
    durable.copy_dispatches.clear(); // Dispatch diagnostics are bounded process-local views.
    assert_eq!(harness.snapshot().expect("recovered"), durable);
}

#[test]
fn replay_failed_broker_submit_does_not_publish_reserved_request() {
    let directory = TestDirectory::new("broker-reserved-commit");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let account_id = TradingAccountId::try_new("replay-demo").expect("id");
    harness
        .register_account(TradingAccount {
            id: account_id.clone(),
            display_name: "replay demo".into(),
            environment: AccountEnvironment::Demo,
            venue_id: "ctrader".into(),
            broker_ref: Some("sanitized".into()),
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("demo account");
    let (_, requests) = harness.command(Command::AttachVenue).expect("attach venue");
    assert!(matches!(
        requests.try_recv(),
        Ok(crate::venue::VenueRequest::Reconcile { .. })
    ));
    let mut order = market_order("broker-commit", OrderSide::Buy, 1, 1_000);
    order.account_id = account_id;
    let before = harness.snapshot().expect("before");
    harness.crash_during_commit(1);
    assert!(harness.place_order(order.clone()).is_err());
    assert_eq!(harness.snapshot().expect("memory"), before);
    assert!(matches!(
        requests.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    harness.restart().expect("restart after failure");
    assert_eq!(harness.snapshot().expect("disk"), before);
    // The old receiver belongs to the retired generation. Reattach before replaying.
    let (_, next) = harness
        .command(Command::AttachVenue)
        .expect("new generation");
    assert!(matches!(
        next.try_recv(),
        Ok(crate::venue::VenueRequest::Reconcile { .. })
    ));
    harness.place_order(order).expect("replay once");
    assert!(matches!(
        next.try_recv(),
        Ok(crate::venue::VenueRequest::Place(_))
    ));
}

#[test]
fn replay_failed_cancel_all_rolls_back_bracket_and_multiple_orders() {
    let directory = TestDirectory::new("cancel-all-bracket");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let mut entry = market_order("pending-bracket", OrderSide::Buy, 1, 1_000);
    entry.order_type = OrderType::Limit;
    entry.time_in_force = TimeInForce::GoodTillCancelled;
    entry.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
    harness
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: BracketStrategyTemplate {
                template_id: "cancel-template".into(),
                revision: 1,
                name: "Cancel bracket".into(),
                stop_offset_ticks: 8,
                targets: vec![BracketTarget {
                    offset_ticks: 8,
                    quantity_percent: 100,
                }],
                trailing_stop: None,
                break_even: None,
                enabled: true,
            },
        })
        .expect("bracket");
    let mut other = market_order("another-order", OrderSide::Buy, 2, 2_000);
    other.order_type = OrderType::Limit;
    other.time_in_force = TimeInForce::GoodTillCancelled;
    other.limit_price = Some(FixedPoint::try_new(8_900, 2).expect("price"));
    harness.place_order(other).expect("second order");
    let before = harness.snapshot().expect("before");
    let cancel =
        |harness: &mut ReplayHarness| harness.command(|reply| Command::CancelAll(None, reply));
    harness.crash_during_commit(1);
    assert!(cancel(&mut harness).is_err());
    assert_eq!(harness.snapshot().expect("memory"), before);
    harness.restart().expect("restart");
    assert_eq!(harness.snapshot().expect("disk"), before);
    harness.crash_during_commit(2);
    assert_eq!(cancel(&mut harness).expect("one transaction").len(), 2);
    let after = harness.snapshot().expect("after");
    assert!(after.orders.iter().all(|order| !order.status.is_open()));
    harness.restart().expect("restart committed");
    assert_eq!(harness.snapshot().expect("recovered"), after);
}

#[test]
fn replay_failed_cancel_and_amend_keep_memory_and_disk_at_last_commit() {
    for amend in [false, true] {
        let directory = TestDirectory::new("replay-revision-commit");
        let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
        let mut harness = start_golden_service(&directory, clock);
        let mut pending = market_order("revision-order", OrderSide::Buy, 1, 1_000);
        pending.order_type = OrderType::Limit;
        pending.time_in_force = TimeInForce::GoodTillCancelled;
        pending.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
        harness.place_order(pending).expect("place");
        let before = harness.transcript();
        harness.crash_during_commit(2);
        let apply = |harness: &mut ReplayHarness| {
            if amend {
                harness
                    .modify_order(ModifyOrder {
                        client_order_id: ClientOrderId::try_new("revision-order").expect("id"),
                        time_in_force: TimeInForce::GoodTillCancelled,
                        limit_price: Some(FixedPoint::try_new(8_900, 2).expect("price")),
                        stop_price: None,
                        modified_unix_nanos: 5_000,
                        provenance: provenance(3, 5_000),
                    })
                    .map(|_| ())
            } else {
                harness
                    .cancel_order(ClientOrderId::try_new("revision-order").expect("id"))
                    .map(|_| ())
            }
        };
        apply(&mut harness).expect("one atomic commit, not two");
        let committed = harness.transcript();
        assert_ne!(committed, before);
        harness.restart().expect("reopen same database");
        assert_eq!(harness.transcript(), committed, "memory and disk diverged");
    }
}

#[test]
fn replay_cancel_and_amend_first_commit_failure_rolls_back_revision() {
    for amend in [false, true] {
        let directory = TestDirectory::new("replay-order-rollback");
        let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
        let mut harness = start_golden_service(&directory, clock);
        let mut pending = market_order("rollback-order", OrderSide::Buy, 1, 1_000);
        pending.order_type = OrderType::Limit;
        pending.time_in_force = TimeInForce::GoodTillCancelled;
        pending.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("price"));
        harness.place_order(pending).expect("place");
        let before = harness.transcript();
        harness.crash_during_commit(1);
        let apply = |harness: &mut ReplayHarness| {
            if amend {
                harness.modify_order(ModifyOrder {
                    client_order_id: ClientOrderId::try_new("rollback-order").expect("id"),
                    time_in_force: TimeInForce::GoodTillCancelled,
                    limit_price: Some(FixedPoint::try_new(8_900, 2).expect("price")),
                    stop_price: None,
                    modified_unix_nanos: 5_000,
                    provenance: provenance(3, 5_000),
                })
            } else {
                harness.cancel_order(ClientOrderId::try_new("rollback-order").expect("id"))
            }
        };
        assert!(apply(&mut harness).is_err(), "first commit must fail");
        assert_eq!(harness.transcript(), before, "rollback changed memory");
        harness.restart().expect("reopen after rollback");
        assert_eq!(harness.transcript(), before, "rollback changed disk");
        apply(&mut harness).expect("replay succeeds after rollback");
        let committed = harness.transcript();
        harness.restart().expect("reopen committed order");
        assert_eq!(harness.transcript(), committed);
    }
}

#[test]
fn mark_only_quotes_never_write_sqlite_and_update_memory_pnl() {
    let directory = TestDirectory::new("mark-only-writes");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    harness
        .place_order(market_order("mark-entry", OrderSide::Buy, 1, 1_000))
        .expect("entry");
    harness
        .observe_market(observation(10_000, 10_025, 2, 2_000))
        .expect("entry fills");
    let mut resting = market_order("mark-resting", OrderSide::Buy, 3, 3_000);
    resting.order_type = OrderType::Limit;
    resting.time_in_force = TimeInForce::GoodTillCancelled;
    resting.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("limit"));
    harness.place_order(resting).expect("resting");
    let before = harness.snapshot().expect("before");
    let database = rusqlite::Connection::open(&harness.configuration.database_path)
        .expect("second connection");
    let version: i64 = database
        .pragma_query_value(None, "data_version", |row| row.get(0))
        .expect("version");
    let changes = harness.current().store.write_count();
    harness.crash_during_commit(1);
    for sequence in 3..1_003_u64 {
        harness
            .observe_market(observation(
                10_001 + i64::try_from(sequence % 2).expect("bounded mark"),
                10_025,
                sequence,
                i64::try_from(sequence).expect("bounded sequence") * 1_000,
            ))
            .expect("mark-only quote must not commit");
    }
    let after = harness.snapshot().expect("after");
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.fills, before.fills);
    assert_ne!(
        after.positions[0].unrealized_pnl,
        before.positions[0].unrealized_pnl
    );
    assert_eq!(harness.current().store.write_count(), changes);
    assert_eq!(
        database
            .pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
            .expect("version"),
        version
    );
    harness.crash_during_commit(2);
    harness
        .observe_market(observation(8_975, 9_000, 1_003, 1_003_000))
        .expect("crossed order commits once");
    let filled = harness.snapshot().expect("filled snapshot");
    assert_eq!(filled.fills.len(), before.fills.len() + 1);
    assert_eq!(filled.revision, before.revision + 1);
    assert!(
        database
            .pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
            .expect("committed version")
            > version
    );
}

#[test]
#[ignore = "release throughput measurement; run explicitly with --nocapture"]
fn benchmark_quotes_with_one_open_position() {
    let directory = TestDirectory::new("quote-throughput");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    harness
        .place_order(market_order("benchmark-entry", OrderSide::Buy, 1, 1_000))
        .expect("entry accepted");
    harness
        .observe_market(observation(10_000, 10_025, 2, 2_000))
        .expect("entry fills");
    assert_eq!(harness.snapshot().expect("position").positions.len(), 1);
    let count = 1_000_u32;
    let start = std::time::Instant::now();
    for sequence in 3..count + 3 {
        harness
            .observe_market(observation(
                10_000 + i64::from(sequence % 2),
                10_025 + i64::from(sequence % 2),
                u64::from(sequence),
                i64::from(sequence) * 1_000,
            ))
            .expect("mark quote");
    }
    let elapsed = start.elapsed();
    println!(
        "quote-throughput: {count} quotes in {elapsed:?}; {:.1} quotes/s",
        f64::from(count) / elapsed.as_secs_f64()
    );
}

#[test]
fn replay_seed_and_scenario_are_byte_deterministic() {
    let scenario = |fail_at| {
        let directory = TestDirectory::new("replay-determinism");
        let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
        let mut harness = start_golden_service(&directory, clock);
        match fail_at {
            None => harness.run_seeded(0x1234_5678, 64),
            Some(step) => harness.run_seeded_with_check(0x1234_5678, 64, |index, _| index != step),
        }
    };
    assert_eq!(scenario(None), scenario(None));
    let seed = 0x1234_5678;
    let failure = || {
        let panic = std::panic::catch_unwind(|| scenario(Some(7))).expect_err("injected failure");
        *panic.downcast::<String>().expect("seeded failure message")
    };
    let first = failure();
    assert!(first.contains(&format!("seed={seed} step=7")));
    assert_eq!(first, failure(), "rerunning seed reproduces failure");
}
