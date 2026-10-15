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
                    let before = self.transcript();
                    self.restart().unwrap_or_else(|error| {
                        panic!("replay seed={seed} step={step} restart: {error}")
                    });
                    assert_eq!(
                        self.transcript(),
                        before,
                        "replay seed={seed} step={step}: recovery"
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
        include_str!("../tests/simulated_golden.txt").trim_end()
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
        include_str!("../tests/simulated_stop_golden.txt").trim_end()
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
