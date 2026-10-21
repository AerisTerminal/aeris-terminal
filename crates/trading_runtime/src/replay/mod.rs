//! Frozen simulated-venue behavior, recorded before broker venue routing was introduced.
//! Do not update the expected ledger to accommodate a simulated-path refactor.

use super::*;
use aeris_trading::market::{
    FeedState, InstrumentTerms, MarketFeedEvent, MarketRoute, TradingQuote,
};
use aeris_trading::{FeeModel, LedgerEntryKind};
use std::fmt::Write;

/// Single-threaded replay of the production core against a real, private SQLite file.
struct ReplayHarness {
    core: Option<TradingCore>,
    configuration: TradingServiceConfig,
    feed_ready: bool,
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
            feed_ready: false,
        })
    }

    fn restart(&mut self) -> Result<(), StoreError> {
        // Releasing the SQLite connection is part of the restart, not merely reloading state.
        drop(self.core.take());
        self.core = Self::new(self.configuration.clone())?.core;
        self.feed_ready = false;
        Ok(())
    }

    /// Delivers one quote through the real market-event path so flatten and reverse price from
    /// the owner's verified quote book exactly like production; the direct-observation command
    /// bypasses the book and cannot price them.
    fn feed_quote(
        &mut self,
        instrument: &TradingInstrument,
        observation: &SimulatedMarketObservation,
    ) -> Result<(), String> {
        let id = observation.instrument_id.clone();
        let mut controls = Vec::new();
        if !self.feed_ready {
            controls.push(MarketFeedEvent::Terms(InstrumentTerms {
                route: MarketRoute {
                    provider_id: "replay-feed".to_string(),
                    instrument_id: id.clone(),
                    provider_symbol: "REPLAY".to_string(),
                    display_symbol: "REPLAY".to_string(),
                    venue_id: "REPLAY".to_string(),
                    entitlement_id: None,
                    price_scale: 2,
                    quantity_scale: 0,
                },
                instrument_class: instrument.contract.instrument_class,
                price_grid: instrument.contract.price_grid,
                point_value: instrument.contract.point_value,
                quantity_increment: instrument.contract.order_quantity_increment,
                currency: instrument.contract.currency.clone(),
                expiry: instrument.contract.expiry,
                first_notice: instrument.contract.first_notice,
                last_trade: instrument.contract.last_trade,
                session_hours: instrument.contract.session_hours.clone(),
                session_generation: 1,
            }));
            controls.push(super::market_feed::live_session(id.clone(), 1));
            controls.push(super::market_feed::feed_state(
                id.clone(),
                1,
                FeedState::Live,
            ));
        }
        let quote = TradingQuote {
            instrument_id: id,
            bid: observation.bid,
            ask: observation.ask,
            bid_size: None,
            ask_size: None,
            last: None,
            provider_id: "replay-feed".to_string(),
            provider_generation: 1,
            source_sequence: Some(observation.provenance.source_sequence),
            exchange_unix_nanos: None,
            received_unix_nanos: observation.provenance.observed_unix_nanos,
        };
        let core = self
            .core
            .as_mut()
            .ok_or_else(|| "replay core is stopped".to_string())?;
        core.apply_market_batch(crate::core::market::feed::MarketFeedBatch {
            quotes: vec![quote],
            controls,
            resync: false,
        });
        self.feed_ready = true;
        Ok(())
    }

    fn current(&self) -> &TradingCore {
        self.core.as_ref().expect("replay core is open")
    }

    fn crash_during_commit(&mut self, nth: usize) {
        assert!(nth > 0);
        self.current().store.crash_during_commit(nth);
    }

    fn venue_event(&mut self, event: &crate::venue::VenueEvent) -> Result<(), String> {
        self.core
            .as_mut()
            .expect("replay core is open")
            .apply_venue_event(event)
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

    fn register_risk_profile(&mut self, profile: RiskProfile) -> Result<(), String> {
        self.command(|reply| Command::RegisterRiskProfile(profile, reply))
    }

    fn set_fee_schedule(
        &mut self,
        account_id: TradingAccountId,
        schedule: aeris_trading::FeeSchedule,
    ) -> Result<(), String> {
        self.command(|reply| Command::SetFeeSchedule(account_id, schedule, reply))
    }

    fn place_order(&mut self, order: PlaceOrder) -> Result<Order, String> {
        self.command(|reply| Command::Place(order, reply))
    }

    fn place_inline_bracket(
        &mut self,
        bracket: PlaceInlineBracket,
    ) -> Result<PlacementOutcome, String> {
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

    fn place_oco(&mut self, pair: PlaceOco) -> Result<[Order; 2], String> {
        self.command(|reply| Command::PlaceOco(pair, reply))
    }
    fn cancel_order(&mut self, id: ClientOrderId) -> Result<Order, String> {
        self.command(|reply| Command::Cancel(id, reply))
    }

    fn flatten(&mut self, scope: FlattenScope) -> Result<FlattenOutcome, String> {
        self.command(|reply| Command::Flatten(scope, reply))
    }

    fn reverse_position(
        &mut self,
        id: TradingAccountId,
        instrument: InstrumentId,
    ) -> Result<FlattenOutcome, String> {
        self.command(|reply| Command::ReverseAt(id, instrument, reply))
    }

    fn kill_switch(
        &mut self,
        id: Option<TradingAccountId>,
        reason: String,
        at: i64,
    ) -> Result<EmergencyOutcome, String> {
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
            "event {} {} {} {} {} {:?} {} {}",
            event.id.as_str(),
            event.order_id.as_str(),
            event.sequence,
            event.kind.as_str(),
            event.event_unix_nanos,
            event.detail,
            event
                .reason
                .map_or("-", aeris_trading::OrderEventReason::as_str),
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
    write_ledger_entries(&mut result, snapshot);
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

/// The durable cash-ledger chain, one line per entry in commit order.
fn write_ledger_entries(result: &mut String, snapshot: &TradingSnapshot) {
    for entry in &snapshot.ledger_entries {
        writeln!(
            result,
            "ledger {} {} {} {} {} {}",
            entry.entry_id,
            entry.account_id.as_str(),
            entry.kind.as_str(),
            point(entry.amount),
            entry.fill_id.as_ref().map_or("-", |id| id.as_str()),
            entry.at_unix_nanos
        )
        .expect("write ledger entry");
    }
}

fn start_golden_service(
    directory: &TestDirectory,
    clock: std::sync::Arc<crate::clock::ManualClock>,
) -> ReplayHarness {
    start_golden_service_with_instrument(directory, clock, instrument())
}

fn start_golden_service_with_instrument(
    directory: &TestDirectory,
    clock: std::sync::Arc<crate::clock::ManualClock>,
    instrument: TradingInstrument,
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
        .register_instrument(instrument)
        .expect("instrument registers");
    service
}

fn crypto_replay_harness(
    directory: &TestDirectory,
    clock: std::sync::Arc<crate::clock::ManualClock>,
) -> (ReplayHarness, TradingInstrument) {
    let mut crypto = instrument();
    crypto.instrument_id = InstrumentId::try_new("instrument:fixture:crypto:BTC").expect("crypto");
    crypto.contract.instrument_class = aeris_instruments::InstrumentClass::CryptoPerpetual;
    crypto.contract.point_value = Some(InstrumentDecimal::try_new(100, 2).expect("point value"));
    crypto.contract.expiry = None;
    crypto.contract.last_trade = None;
    crypto.contract.session_hours.clear();
    crypto.contract.provenance.provider_symbol = "BTC".to_string();
    crypto.contract.provenance.display_symbol = "BTC".to_string();
    let mut harness = start_golden_service_with_instrument(directory, clock, crypto.clone());
    let account_id = TradingAccountId::try_new("aeris-sim-1").expect("account");
    harness
        .register_risk_profile(RiskProfile {
            account_id: account_id.clone(),
            profile_id: "replay-risk".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("zero"),
            daily_loss_limit: FixedPoint::try_new(100_000, 2).expect("limit"),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::Intraday,
            max_contracts: FixedPoint::try_new(10, 0).expect("contracts"),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("risk profile");
    (harness, crypto)
}

fn crypto_replay_quote(
    crypto: &TradingInstrument,
    bid: i64,
    ask: i64,
    sequence: u64,
    time: i64,
) -> SimulatedMarketObservation {
    SimulatedMarketObservation {
        instrument_id: crypto.instrument_id.clone(),
        bid: FixedPoint::try_new(bid, 2).expect("bid"),
        ask: FixedPoint::try_new(ask, 2).expect("ask"),
        provenance: provenance(sequence, time),
    }
}

fn assert_crypto_fill_accounting(harness: &ReplayHarness, fill: &Fill, expected: FixedPoint) {
    let database = rusqlite::Connection::open(&harness.configuration.database_path)
        .expect("accounting reader");
    let stored_realized: i64 = database
        .query_row(
            "SELECT realized_gross_units FROM fill_accounting WHERE fill_id = ?1",
            [fill.id.as_str()],
            |row| row.get(0),
        )
        .expect("durable fill accounting");
    assert_eq!(stored_realized, expected.units());
}

fn crypto_stop_restart_accounting_scenario() -> String {
    let directory = TestDirectory::new("crypto-stop-restart-golden");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let (mut harness, crypto) = crypto_replay_harness(&directory, clock.clone());
    let mut trace = String::new();
    let entry_quote = crypto_replay_quote(&crypto, 10_000, 10_025, 1, 1_000);
    harness
        .observe_market(entry_quote.clone())
        .expect("fresh quote");
    let mut entry = market_order("crypto-entry", OrderSide::Buy, 2, 2_000);
    entry.instrument_id = crypto.instrument_id.clone();
    harness.place_order(entry).expect("buy accepted");
    let entry_fills = harness
        .observe_market(crypto_replay_quote(&crypto, 10_000, 10_025, 3, 3_000))
        .expect("market buy fills");
    assert_eq!(entry_fills.len(), 1);
    assert_eq!(
        entry_fills[0].price,
        FixedPoint::try_new(10_025, 2).expect("ask")
    );
    record("market buy at ask", &harness, &mut trace);

    let mut stop = market_order("crypto-stop", OrderSide::Sell, 4, 4_000);
    stop.instrument_id = crypto.instrument_id.clone();
    stop.order_type = OrderType::Stop;
    stop.time_in_force = TimeInForce::GoodTillCancelled;
    stop.stop_price = Some(FixedPoint::try_new(9_800, 2).expect("stop"));
    harness.place_order(stop).expect("stop accepted");
    record("sell stop working", &harness, &mut trace);
    let before_restart = harness.snapshot().expect("before first restart");
    harness.restart().expect("first restart");
    record("restart without chart demand", &harness, &mut trace);
    assert_eq!(
        harness.snapshot().expect("restored").orders,
        before_restart.orders
    );
    assert_eq!(
        harness
            .observe_market(crypto_replay_quote(&crypto, 10_000, 10_025, 3, 3_000))
            .expect("retired quote"),
        []
    );
    assert_eq!(
        harness.snapshot().expect("stale").fills,
        before_restart.fills
    );
    assert_eq!(
        harness.snapshot().expect("stale").orders,
        before_restart.orders
    );
    assert_eq!(
        harness.snapshot().expect("stale").revision,
        before_restart.revision
    );
    record("stale pre-restart quote ignored", &harness, &mut trace);

    clock.set_wall_unix_nanos(5_000);
    let exit_fills = harness
        .observe_market(crypto_replay_quote(&crypto, 9_750, 9_775, 5, 5_000))
        .expect("new stop quote");
    assert_eq!(exit_fills.len(), 1);
    assert_eq!(
        exit_fills[0].price,
        FixedPoint::try_new(9_750, 2).expect("bid")
    );
    let closed = harness.snapshot().expect("flat snapshot");
    let expected =
        FixedPoint::try_new(-275, 2).expect("bid minus ask times quantity and point value");
    assert_eq!(closed.positions[0].net_quantity.units(), 0);
    assert_eq!(closed.positions[0].realized_pnl, expected);
    assert_eq!(
        closed.fill_realized_pnl.get(&exit_fills[0].id),
        Some(&expected)
    );
    assert_eq!(closed.risk_meters[0].current_realized_pnl, expected);
    assert_eq!(closed.account_pnl[0].realized, expected);
    assert_crypto_fill_accounting(&harness, &exit_fills[0], expected);
    record("stop filled at bid and flat", &harness, &mut trace);
    let committed = harness.snapshot().expect("before second restart");
    harness.restart().expect("second restart");
    let recovered = harness.snapshot().expect("second recovery");
    assert_eq!(
        (
            recovered.orders,
            recovered.order_events,
            recovered.fills,
            recovered.positions,
            recovered.revision
        ),
        (
            committed.orders,
            committed.order_events,
            committed.fills,
            committed.positions,
            committed.revision
        )
    );
    record("second restart preserves facts", &harness, &mut trace);
    format!("{trace}\n{}", harness.transcript())
}

#[test]
fn replay_crypto_stop_restart_accounting_golden() {
    let first = crypto_stop_restart_accounting_scenario();
    assert_eq!(first, crypto_stop_restart_accounting_scenario());
    assert_eq!(
        first.trim_end(),
        include_str!("../tests/replay_crypto_stop_restart_accounting_golden_v2.txt")
            .replace("\r\n", "\n")
            .trim_end()
    );
}

/// One per-contract fee schedule for the fixture future class.
fn per_contract_schedule(per_side_units: i64) -> aeris_trading::FeeSchedule {
    let mut schedule = aeris_trading::FeeSchedule::default();
    schedule.by_class.insert(
        aeris_instruments::InstrumentClass::Future,
        FeeModel::PerContract {
            per_side: FixedPoint::try_new(per_side_units, 2).expect("fee"),
        },
    );
    schedule
}

/// The ledger chain equals the expected (kind, amount-units) sequence exactly.
fn assert_ledger_chain(harness: &ReplayHarness, expected: &[(LedgerEntryKind, i64)]) {
    let chain = harness
        .snapshot()
        .expect("ledger snapshot")
        .ledger_entries
        .iter()
        .map(|entry| (entry.kind, entry.amount.units()))
        .collect::<Vec<_>>();
    assert_eq!(chain, expected);
}

/// The exact fee-charged round trip: a funded account pays a per-contract commission on the
/// taker entry and exit and banks the exact realized P&L between them.
fn fee_round_trip_scenario() -> String {
    let directory = TestDirectory::new("fee-round-trip-golden");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock.clone());
    harness
        .set_fee_schedule(
            TradingAccountId::try_new("aeris-sim-1").expect("account"),
            per_contract_schedule(125),
        )
        .expect("fee schedule");
    let mut trace = String::new();
    harness
        .observe_market(observation(10_000, 10_025, 1, 1_000))
        .expect("fresh quote");
    harness
        .place_order(market_order("fee-entry", OrderSide::Buy, 2, 2_000))
        .expect("buy accepted");
    let entry_fills = harness
        .observe_market(observation(10_000, 10_025, 3, 3_000))
        .expect("entry fill");
    assert_eq!(entry_fills.len(), 1);
    assert_eq!(
        entry_fills[0].price,
        FixedPoint::try_new(10_025, 2).expect("ask")
    );
    record("taker entry pays commission", &harness, &mut trace);

    clock.set_wall_unix_nanos(6_000);
    harness
        .place_order(market_order("fee-exit", OrderSide::Sell, 4, 4_000))
        .expect("sell accepted");
    let exit_fills = harness
        .observe_market(observation(10_050, 10_075, 5, 5_000))
        .expect("exit fill");
    assert_eq!(exit_fills.len(), 1);
    assert_eq!(
        exit_fills[0].price,
        FixedPoint::try_new(10_050, 2).expect("bid")
    );
    let flat = harness.snapshot().expect("flat");
    assert_eq!(flat.positions[0].net_quantity.units(), 0);
    assert_eq!(
        flat.positions[0].realized_pnl,
        FixedPoint::try_new(1_250, 2).expect("exact realized")
    );
    assert_eq!(
        flat.account_pnl[0].balance,
        Some(FixedPoint::try_new(5_001_000, 2).expect("balance"))
    );
    assert_ledger_chain(
        &harness,
        &[
            (LedgerEntryKind::Deposit, 5_000_000),
            (LedgerEntryKind::Commission, -125),
            (LedgerEntryKind::RealizedPnl, 1_250),
            (LedgerEntryKind::Commission, -125),
        ],
    );
    record(
        "taker exit banks realized net of fees",
        &harness,
        &mut trace,
    );

    let committed = harness.snapshot().expect("before restart");
    harness.restart().expect("restart");
    let recovered = harness.snapshot().expect("recovered");
    assert_eq!(recovered.ledger_entries, committed.ledger_entries);
    assert_eq!(recovered.account_pnl, committed.account_pnl);
    assert_ledger_chain(
        &harness,
        &[
            (LedgerEntryKind::Deposit, 5_000_000),
            (LedgerEntryKind::Commission, -125),
            (LedgerEntryKind::RealizedPnl, 1_250),
            (LedgerEntryKind::Commission, -125),
        ],
    );
    record(
        "restart replays the exact ledger chain",
        &harness,
        &mut trace,
    );
    format!("{trace}\n{}", harness.transcript())
}

#[test]
fn replay_fee_round_trip_ledger_golden() {
    let first = fee_round_trip_scenario();
    assert_eq!(first, fee_round_trip_scenario());
    assert_eq!(
        first.trim_end(),
        include_str!("../tests/replay_fee_round_trip_ledger_golden.txt")
            .replace("\r\n", "\n")
            .trim_end()
    );
}
/// A restart must reproduce the durable facts exactly and raise no new recovery notice.
fn assert_restart_preserves(before: &TradingSnapshot, after: &TradingSnapshot) {
    assert_eq!(
        (
            &after.orders,
            &after.order_events,
            &after.fills,
            &after.positions,
            after.revision,
            &after.recovery_notices,
        ),
        (
            &before.orders,
            &before.order_events,
            &before.fills,
            &before.positions,
            before.revision,
            &before.recovery_notices,
        ),
        "a restart reproduces the durable facts"
    );
}

/// The durable group status of one bracket in the replay store.
fn assert_group_status(harness: &ReplayHarness, group_id: &str, expected: &str) {
    let database =
        rusqlite::Connection::open(&harness.configuration.database_path).expect("group reader");
    let status: String = database
        .query_row(
            "SELECT status FROM order_groups WHERE group_id=?1",
            [group_id],
            |row| row.get(0),
        )
        .expect("group row");
    assert_eq!(status, expected);
}

/// An active bracket survives a clean restart: the recovered stop fills on the first
/// eligible post-restart quote, cascades its targets and completes the group, and a second
/// restart replays the facts identically.
fn bracket_stop_after_restart_scenario() -> String {
    let directory = TestDirectory::new("bracket-stop-restart-golden");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock.clone());
    let mut trace = String::new();
    harness
        .observe_market(observation(9_975, 10_000, 1, 1_000))
        .expect("fresh quote");
    let mut entry = market_order("golden-bracket", OrderSide::Buy, 2, 2_000);
    entry.quantity = quantity(2);
    harness
        .place_inline_bracket(PlaceInlineBracket {
            entry,
            template: BracketStrategyTemplate {
                template_id: "golden-bracket".into(),
                revision: 1,
                name: "Golden stop and targets".to_string(),
                stop_offset_ticks: 8,
                targets: vec![
                    BracketTarget {
                        offset_ticks: 8,
                        quantity_percent: 50,
                    },
                    BracketTarget {
                        offset_ticks: 16,
                        quantity_percent: 50,
                    },
                ],
                trailing_stop: None,
                break_even: None,
                enabled: true,
            },
        })
        .expect("bracket placed");
    let entry_fills = harness
        .observe_market(observation(9_975, 10_000, 3, 3_000))
        .expect("entry fills");
    assert_eq!(entry_fills.len(), 1, "the market entry fills at the ask");
    assert_group_status(&harness, "golden-bracket", "active");
    record("bracket active after entry fill", &harness, &mut trace);

    let before_restart = harness.snapshot().expect("before restart");
    harness.restart().expect("restart with an active bracket");
    assert_restart_preserves(&before_restart, &harness.snapshot().expect("recovered"));
    record("restart preserves the active bracket", &harness, &mut trace);

    // The first post-restart quote fills the recovered stop at the bid and
    // cascades both targets in the same step.
    let exit_fills = harness
        .observe_market(observation(9_750, 9_775, 4, 4_000))
        .expect("stop fills after restart");
    assert_eq!(exit_fills.len(), 1, "the recovered stop fills");
    assert_eq!(
        exit_fills[0].price,
        FixedPoint::try_new(9_750, 2).expect("bid")
    );
    let flat = harness.snapshot().expect("flat snapshot");
    assert_eq!(flat.positions[0].net_quantity.units(), 0);
    assert!(
        !flat.orders.iter().any(|order| order.status.is_open()),
        "no leg survives the stop"
    );
    assert_group_status(&harness, "golden-bracket", "completed");
    record(
        "stop fills after restart and the group completes",
        &harness,
        &mut trace,
    );

    let committed = harness.snapshot().expect("before second restart");
    harness.restart().expect("second restart");
    assert_restart_preserves(&committed, &harness.snapshot().expect("second recovery"));
    record("second restart preserves facts", &harness, &mut trace);
    format!("{trace}\n{}", harness.transcript())
}

#[test]
fn replay_bracket_stop_after_restart_golden() {
    let first = bracket_stop_after_restart_scenario();
    assert_eq!(first, bracket_stop_after_restart_scenario());
    assert_eq!(
        first.trim_end(),
        include_str!("../tests/replay_bracket_stop_after_restart_golden_v2.txt")
            .replace("\r\n", "\n")
            .trim_end()
    );
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
        BracketViewStatus::Active
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
        BracketViewStatus::Completed
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
        .feed_quote(&instrument(), &observation(10_100, 10_125, 6, 6_000))
        .expect("flatten quote");
    service
        .flatten(FlattenScope {
            account: Some(account.clone()),
            instrument: None,
        })
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
        .feed_quote(&instrument(), &observation(10_100, 10_125, 9, 9_000))
        .expect("bracket flatten quote");
    service
        .flatten(FlattenScope {
            account: Some(account.clone()),
            instrument: None,
        })
        .expect("flatten bracket position");
    record("bracket flatten", &service, &mut trace);

    service
        .place_order(market_order("reverse-entry", OrderSide::Buy, 10, 10_000))
        .expect("reverse entry accepted");
    service
        .observe_market(observation(10_000, 10_025, 11, 11_000))
        .expect("reverse entry fills");
    service
        .feed_quote(&instrument(), &observation(10_100, 10_125, 12, 12_000))
        .expect("reverse quote");
    service
        .reverse_position(account.clone(), instrument().instrument_id)
        .expect("reverse position");
    record("reverse", &service, &mut trace);

    let mut pending = market_order("kill-pending", OrderSide::Buy, 13, 13_000);
    pending.order_type = OrderType::Limit;
    pending.time_in_force = TimeInForce::GoodTillCancelled;
    pending.limit_price = Some(FixedPoint::try_new(9_000, 2).expect("pending price"));
    service.place_order(pending).expect("pending order");
    clock.set_wall_unix_nanos(13_000);
    // The kill switch locks the account, cancels the pending limit with the kill-switch
    // cause and flattens the reversed short at the verified quote in the same step.
    let killed = service
        .kill_switch(Some(account), "golden kill".to_string(), 14_000)
        .expect("kill switch");
    assert_eq!(killed.accounts_locked, 1);
    assert_eq!(killed.cancelled.len(), 1);
    assert_eq!(killed.fills.len(), 1);
    assert_eq!(killed.unpriced, []);
    record("kill switch", &service, &mut trace);

    let actual = format!(
        "{trace}\n{}",
        ledger(&service.snapshot().expect("final snapshot"))
    );
    actual
}

#[test]
fn replay_failed_flatten_commit_keeps_memory_and_disk_at_last_commit() {
    let directory = TestDirectory::new("flatten-atomic");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    harness
        .place_order(market_order("atomic-open", OrderSide::Buy, 1, 1_000))
        .expect("buy accepted");
    harness
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("buy fills");
    harness
        .feed_quote(&instrument(), &observation(10_100, 10_125, 3, 3_000))
        .expect("flatten quote");
    let before = harness.snapshot().expect("before flatten");
    harness.crash_during_commit(1);
    assert!(
        harness
            .flatten(FlattenScope {
                account: Some(account.clone()),
                instrument: None,
            })
            .is_err(),
        "injected commit failure rejects the flatten"
    );
    assert_eq!(
        harness.snapshot().expect("memory unchanged"),
        before,
        "a failed flatten commit leaves owner memory untouched"
    );
    harness.restart().expect("restart");
    assert_eq!(
        harness.snapshot().expect("disk unchanged"),
        before,
        "a failed flatten commit leaves the database untouched"
    );
    // The quote book is owner memory: after a restart a fresh verified quote must arrive
    // before the position can be flattened.
    harness
        .feed_quote(&instrument(), &observation(10_100, 10_125, 4, 6_000))
        .expect("post-restart quote");
    let flattened = harness
        .flatten(FlattenScope {
            account: Some(account.clone()),
            instrument: None,
        })
        .expect("flatten succeeds after restart");
    assert_eq!(flattened.fills.len(), 1);
    let after = harness.snapshot().expect("after flatten");
    harness.restart().expect("restart");
    assert_eq!(
        harness.snapshot().expect("durable flatten"),
        after,
        "the committed flatten replays exactly"
    );
}

#[test]
fn replay_failed_reverse_commit_leaves_the_position_untouched() {
    let directory = TestDirectory::new("reverse-atomic");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    harness
        .place_order(market_order("reverse-open", OrderSide::Buy, 1, 1_000))
        .expect("buy accepted");
    harness
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("buy fills");
    harness
        .feed_quote(&instrument(), &observation(10_100, 10_125, 3, 3_000))
        .expect("reverse quote");
    let before = harness.snapshot().expect("before reverse");
    harness.crash_during_commit(1);
    assert!(
        harness
            .reverse_position(account.clone(), instrument().instrument_id)
            .is_err(),
        "injected commit failure rejects the reverse"
    );
    let unchanged = harness.snapshot().expect("memory unchanged");
    assert_eq!(
        unchanged, before,
        "a failed reverse commit leaves owner memory untouched"
    );
    assert_eq!(unchanged.positions[0].net_quantity.units(), 1);
    harness.restart().expect("restart");
    assert_eq!(
        harness.snapshot().expect("disk unchanged"),
        before,
        "a failed reverse commit leaves the database untouched"
    );
    harness
        .feed_quote(&instrument(), &observation(10_100, 10_125, 4, 6_000))
        .expect("post-restart quote");
    let reversed = harness
        .reverse_position(account.clone(), instrument().instrument_id)
        .expect("reverse succeeds after restart");
    assert_eq!(reversed.fills.len(), 2);
    let after = harness.snapshot().expect("after reverse");
    assert_eq!(after.positions[0].net_quantity.units(), -1);
    harness.restart().expect("restart");
    assert_eq!(
        harness.snapshot().expect("durable reverse"),
        after,
        "the committed reverse replays exactly"
    );
}

#[test]
fn replay_failed_kill_switch_commit_keeps_memory_and_disk_at_last_commit() {
    let directory = TestDirectory::new("kill-atomic");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    harness
        .place_order(market_order("kill-atomic-open", OrderSide::Buy, 1, 1_000))
        .expect("buy accepted");
    harness
        .observe_market(observation(9_975, 10_000, 2, 2_000))
        .expect("buy fills");
    harness
        .feed_quote(&instrument(), &observation(10_100, 10_125, 3, 3_000))
        .expect("kill quote");
    let before = harness.snapshot().expect("before kill");
    harness.crash_during_commit(1);
    assert!(
        harness
            .kill_switch(Some(account.clone()), "atomic kill".to_string(), 4_000)
            .is_err(),
        "injected commit failure rejects the kill switch"
    );
    assert_eq!(
        harness.snapshot().expect("memory unchanged"),
        before,
        "a failed kill commit leaves owner memory untouched, including the lock"
    );
    harness.restart().expect("restart");
    assert_eq!(
        harness.snapshot().expect("disk unchanged"),
        before,
        "a failed kill commit leaves the database unlocked and unflattened"
    );
    // The quote book is owner memory: after a restart a fresh verified quote must arrive
    // before the position can be flattened.
    harness
        .feed_quote(&instrument(), &observation(10_100, 10_125, 4, 6_000))
        .expect("post-restart quote");
    let killed = harness
        .kill_switch(Some(account), "atomic kill".to_string(), 7_000)
        .expect("kill switch succeeds after restart");
    assert_eq!(killed.accounts_locked, 1);
    assert_eq!(killed.fills.len(), 1);
    assert_eq!(killed.unpriced, []);
    let after = harness.snapshot().expect("after kill");
    harness.restart().expect("restart");
    assert_eq!(
        harness.snapshot().expect("durable kill"),
        after,
        "the committed kill switch replays exactly"
    );
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
        include_str!("../tests/simulated_golden_v2.txt")
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
        include_str!("../tests/simulated_stop_golden_v2.txt")
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
            venue_id: aeris_trading::venue::BROKER_VENUE_ID.into(),
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
fn broker_report_and_deal_commit_failures_preserve_order_lifecycle_facts() {
    use crate::venue::{VenueEvent, VenueUpdate};
    use aeris_trading::venue::{BrokerFill, BrokerOrder, BrokerOrderKind, BrokerOrderState};

    let directory = TestDirectory::new("broker-report-rollback");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let account_id = TradingAccountId::try_new("replay-demo").expect("id");
    harness
        .register_account(TradingAccount {
            id: account_id.clone(),
            display_name: "replay demo".into(),
            environment: AccountEnvironment::Demo,
            venue_id: aeris_trading::venue::BROKER_VENUE_ID.into(),
            broker_ref: Some("sanitized".into()),
            currency: "USD".into(),
            currency_scale: 2,
            starting_equity: None,
        })
        .expect("account");
    let (generation, requests) = harness.command(Command::AttachVenue).expect("attach");
    assert!(matches!(
        requests.try_recv(),
        Ok(crate::venue::VenueRequest::Reconcile { .. })
    ));
    let mut request = market_order("broker-rollback", OrderSide::Buy, 1, 1_000);
    request.account_id = account_id;
    let placed = harness.place_order(request).expect("place");
    assert!(matches!(
        requests.try_recv(),
        Ok(crate::venue::VenueRequest::Place(_))
    ));
    let event = |generation, update| VenueEvent {
        session_generation: generation,
        broker_account: "sanitized".into(),
        observed_unix_nanos: 5_000,
        update,
    };
    let report = BrokerOrder {
        broker_order_id: "501".into(),
        client_order_id: Some(placed.client_order_id.as_str().into()),
        instrument_id: placed.instrument_id.clone(),
        side: placed.side,
        kind: BrokerOrderKind::Market,
        quantity: placed.quantity,
        filled_quantity: placed.filled_quantity,
        limit_price: None,
        stop_price: None,
        broker_position_id: Some("77".into()),
        closing: false,
    };
    let accepted = event(
        generation,
        VenueUpdate::Order {
            order: report.clone(),
            state: BrokerOrderState::Accepted,
            reason: None,
        },
    );
    assert_failed_venue_commit(&mut harness, &accepted);
    let (generation, _) = harness.command(Command::AttachVenue).expect("reattach");
    harness
        .venue_event(&event(generation, accepted.update))
        .expect("ack");
    let deal = event(
        generation,
        VenueUpdate::Fill(BrokerFill {
            broker_deal_id: "d1".into(),
            broker_order_id: "501".into(),
            broker_position_id: "77".into(),
            instrument_id: placed.instrument_id,
            side: placed.side,
            price: FixedPoint::try_new(9_000, 2).expect("price"),
            quantity: placed.quantity,
            executed_unix_nanos: 5_000,
            commission: None,
            realized: None,
        }),
    );
    assert_failed_venue_commit(&mut harness, &deal);
    let (generation, _) = harness.command(Command::AttachVenue).expect("reattach");
    harness
        .venue_event(&event(generation, deal.update.clone()))
        .expect("deal");
    let committed = harness.snapshot().expect("committed");
    let order = committed
        .orders
        .iter()
        .find(|order| order.id == placed.id)
        .expect("order");
    assert_eq!(
        (order.status, order.revision, order.terminal_unix_nanos),
        (OrderStatus::Filled, 3, Some(5_000))
    );
    harness
        .venue_event(&event(generation, deal.update))
        .expect("replayed deal");
    assert_eq!(harness.snapshot().expect("idempotent replay"), committed);
}

fn assert_failed_venue_commit(harness: &mut ReplayHarness, event: &crate::venue::VenueEvent) {
    let before = harness.snapshot().expect("before failed commit");
    harness.crash_during_commit(1);
    assert!(harness.venue_event(event).is_err());
    assert_eq!(harness.snapshot().expect("memory"), before);
    harness.restart().expect("restart after failed commit");
    assert_eq!(harness.snapshot().expect("disk"), before);
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

/// VAL-TRD-042: an explicit OCO pair under one wide quote that satisfies both members fills
/// exactly one and cancels the sibling in the same step; 100 runs and a restart replay
/// produce the identical transcript.
fn oco_wide_quote_transcript(restart_before_quote: bool) -> String {
    let directory = TestDirectory::new("oco-wide-quote");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    harness
        .observe_market(observation(9_950, 10_000, 1, 1_000))
        .expect("fresh quote");
    let mut buy = market_order("oco-buy", OrderSide::Buy, 2, 2_000);
    buy.order_type = OrderType::Stop;
    buy.time_in_force = TimeInForce::GoodTillCancelled;
    buy.stop_price = Some(FixedPoint::try_new(10_100, 2).expect("stop"));
    let mut sell = market_order("oco-sell", OrderSide::Sell, 3, 3_000);
    sell.order_type = OrderType::Stop;
    sell.time_in_force = TimeInForce::GoodTillCancelled;
    sell.stop_price = Some(FixedPoint::try_new(9_900, 2).expect("stop"));
    harness
        .place_oco(PlaceOco {
            first: buy,
            second: sell,
        })
        .expect("oco pair accepted");
    if restart_before_quote {
        harness.restart().expect("restart before the wide quote");
    }
    let fills = harness
        .observe_market(observation(9_890, 10_110, 4, 4_000))
        .expect("wide quote");
    assert_eq!(
        fills.len(),
        1,
        "exactly one OCO member fills on the wide quote"
    );
    let snapshot = harness.snapshot().expect("snapshot");
    let member = |client: &str| {
        snapshot
            .orders
            .iter()
            .find(|order| order.client_order_id.as_str() == client)
            .expect("oco member")
    };
    let (buy_status, sell_status) = (member("oco-buy").status, member("oco-sell").status);
    assert!(
        matches!(
            (buy_status, sell_status),
            (OrderStatus::Filled, OrderStatus::Cancelled)
                | (OrderStatus::Cancelled, OrderStatus::Filled)
        ),
        "one member fills and its sibling is cancelled: {buy_status:?}/{sell_status:?}"
    );
    let sibling = if buy_status == OrderStatus::Cancelled {
        member("oco-buy")
    } else {
        member("oco-sell")
    };
    // The cancel already shows in the snapshot returned by the same observe_market call,
    // so the sibling never survives into a later quote update.
    assert!(
        snapshot.order_events.iter().any(|event| {
            event.order_id == sibling.id
                && event.kind == OrderEventKind::Cancelled
                && event.reason == Some(aeris_trading::OrderEventReason::Cascade)
        }),
        "the sibling is cancelled with Cascade in the same quote step"
    );
    let database =
        rusqlite::Connection::open(&harness.configuration.database_path).expect("group reader");
    let status: String = database
        .query_row(
            "SELECT status FROM order_groups WHERE kind='oco'",
            [],
            |row| row.get(0),
        )
        .expect("oco group row");
    assert_eq!(status, "completed", "the filled member completes its group");
    drop(database);
    drop(snapshot);
    harness.restart().expect("restart replay");
    harness.transcript()
}

#[test]
fn explicit_oco_wide_quote_cancels_the_sibling_and_replays_identically() {
    let expected = oco_wide_quote_transcript(false);
    for run in 0..100 {
        assert_eq!(
            oco_wide_quote_transcript(false),
            expected,
            "OCO run {run} diverged from the first transcript"
        );
    }
    assert_eq!(
        oco_wide_quote_transcript(true),
        expected,
        "a restart between placement and the wide quote changes nothing"
    );
}

/// VAL-TRD-047 helpers: the protective quantity of one role on one scope, and the coverage
/// invariant checked after every step: each role totals exactly |net position| or zero, no
/// protection guards a flat or wrong-side position, and no group leg is dangling or left
/// open in a terminal group.
fn role_coverage(
    snapshot: &TradingSnapshot,
    account_id: &TradingAccountId,
    instrument_id: &InstrumentId,
    role: ProtectionRole,
) -> i64 {
    snapshot
        .orders
        .iter()
        .filter(|order| {
            order.status.is_open()
                && &order.account_id == account_id
                && &order.instrument_id == instrument_id
                && snapshot.protective_role(&order.client_order_id) == Some(role)
        })
        .map(|order| order.quantity.units().abs())
        .sum()
}

fn assert_coverage_invariant(harness: &ReplayHarness, label: &str) {
    let snapshot = harness.snapshot().expect("snapshot");
    let mut scopes = std::collections::BTreeSet::new();
    for order in &snapshot.orders {
        if order.status.is_open() && snapshot.protective_role(&order.client_order_id).is_some() {
            scopes.insert((order.account_id.clone(), order.instrument_id.clone()));
        }
    }
    for position in &snapshot.positions {
        scopes.insert((position.account_id.clone(), position.instrument_id.clone()));
    }
    for (account_id, instrument_id) in scopes {
        let net = snapshot
            .positions
            .iter()
            .find(|position| {
                position.account_id == account_id && position.instrument_id == instrument_id
            })
            .map_or(0, |position| position.net_quantity.units());
        for role in [ProtectionRole::StopLoss, ProtectionRole::TakeProfit] {
            let total = role_coverage(&snapshot, &account_id, &instrument_id, role);
            assert!(
                total == 0 || total == net.abs(),
                "{label}: {role:?} coverage {total} matches neither zero nor |{net}|"
            );
        }
        if net != 0 {
            for order in &snapshot.orders {
                if order.status.is_open()
                    && order.account_id == account_id
                    && order.instrument_id == instrument_id
                    && snapshot.protective_role(&order.client_order_id).is_some()
                {
                    assert_eq!(
                        order.side.sign(),
                        -net.signum(),
                        "{label}: protective order on the wrong side of its position"
                    );
                }
            }
        }
    }
    let database =
        rusqlite::Connection::open(&harness.configuration.database_path).expect("coverage reader");
    let dangling: i64 = database
        .query_row(
            "SELECT count(*) FROM order_group_legs l
             WHERE NOT EXISTS (SELECT 1 FROM orders o WHERE o.id = l.order_id)",
            [],
            |row| row.get(0),
        )
        .expect("dangling leg count");
    let open_in_terminal: i64 = database
        .query_row(
            "SELECT count(*) FROM order_group_legs l
             JOIN order_groups g ON g.group_id = l.group_id
             JOIN orders o ON o.id = l.order_id
             WHERE g.status IN ('completed','cancelled')
               AND o.status IN ('working','inactive')",
            [],
            |row| row.get(0),
        )
        .expect("orphan leg count");
    assert_eq!(
        (dangling, open_in_terminal),
        (0, 0),
        "{label}: no dangling legs and no open legs in terminal groups"
    );
}

fn two_target_template() -> BracketStrategyTemplate {
    BracketStrategyTemplate {
        template_id: "coverage-template".to_string(),
        revision: 1,
        name: "Coverage".to_string(),
        stop_offset_ticks: 8,
        targets: vec![
            BracketTarget {
                offset_ticks: 8,
                quantity_percent: 50,
            },
            BracketTarget {
                offset_ticks: 16,
                quantity_percent: 50,
            },
        ],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    }
}

fn bracket_entry(client_id: &str, quantity: i64, sequence: u64, time: i64) -> PlaceOrder {
    let mut entry = market_order(client_id, OrderSide::Buy, sequence, time);
    entry.order_type = OrderType::Limit;
    entry.time_in_force = TimeInForce::GoodTillCancelled;
    entry.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    entry.limit_price = Some(FixedPoint::try_new(10_000, 2).expect("entry"));
    entry
}

fn manual_market(client_id: &str, side: OrderSide, quantity: i64, sequence: u64) -> PlaceOrder {
    let time = 10_000 + i64::try_from(sequence).expect("bounded replay steps");
    let mut order = market_order(client_id, side, sequence, time);
    order.quantity = FixedPoint::try_new(quantity, 0).expect("quantity");
    order
}

fn coverage_totals(
    harness: &ReplayHarness,
    account: &TradingAccountId,
    instrument_id: &InstrumentId,
) -> (i64, i64) {
    let snapshot = harness.snapshot().expect("snapshot");
    (
        role_coverage(&snapshot, account, instrument_id, ProtectionRole::StopLoss),
        role_coverage(
            &snapshot,
            account,
            instrument_id,
            ProtectionRole::TakeProfit,
        ),
    )
}

/// VAL-TRD-047: the contract's exact walk: long 4 with stop 4 and targets 2+2; a manual
/// sell of 1 takes both roles to 3; buys of 1 then 2 grow the first-triggering orders so
/// each role totals 6; a reversal through zero completes every group.
#[test]
fn protection_tracks_the_position_through_close_scale_in_and_reversal() {
    let directory = TestDirectory::new("coverage-walk");
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
    let mut harness = start_golden_service(&directory, clock);
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let instrument_id = instrument().instrument_id;
    let quote = |sequence: u64, time: i64| observation(9_975, 10_000, sequence, time);
    harness
        .observe_market(quote(1, 1_000))
        .expect("fresh quote");
    harness
        .place_inline_bracket(PlaceInlineBracket {
            entry: bracket_entry("coverage-entry", 4, 2, 2_000),
            template: two_target_template(),
        })
        .expect("bracket accepted");
    assert_eq!(
        harness
            .observe_market(quote(3, 3_000))
            .expect("entry fill")
            .len(),
        1
    );
    assert_eq!(
        coverage_totals(&harness, &account, &instrument_id),
        (4, 4),
        "stop 4 and targets 2+2 after the fill"
    );
    harness
        .place_order(manual_market("coverage-sell-1", OrderSide::Sell, 1, 4))
        .expect("manual sell accepted");
    harness.observe_market(quote(5, 5_000)).expect("sell fills");
    assert_eq!(
        coverage_totals(&harness, &account, &instrument_id),
        (3, 3),
        "both roles shrink to the position"
    );
    for (step, quantity) in [(6_u64, 1_i64), (8, 2)] {
        harness
            .place_order(manual_market(
                &format!("coverage-buy-{step}"),
                OrderSide::Buy,
                quantity,
                step,
            ))
            .expect("manual buy accepted");
        harness
            .observe_market(quote(
                step + 1,
                10_000 + i64::try_from(step).expect("bounded replay steps"),
            ))
            .expect("buy fills");
    }
    assert_eq!(
        coverage_totals(&harness, &account, &instrument_id),
        (6, 6),
        "both roles grow back to the position"
    );
    assert_reversal_completes_every_group(&mut harness, &account, &instrument_id, &quote);
}

fn assert_reversal_completes_every_group(
    harness: &mut ReplayHarness,
    account: &TradingAccountId,
    instrument_id: &InstrumentId,
    quote: &dyn Fn(u64, i64) -> SimulatedMarketObservation,
) {
    let snapshot = harness.snapshot().expect("snapshot");
    assert!(
        snapshot.order_events.iter().any(|event| {
            event.kind == OrderEventKind::Resized
                && event.reason == Some(aeris_trading::OrderEventReason::CoverageGrow)
        }),
        "scale-in grows protection with a typed event"
    );
    harness
        .place_order(manual_market("coverage-reverse", OrderSide::Sell, 7, 10))
        .expect("reversal accepted");
    harness
        .observe_market(quote(11, 11_000))
        .expect("reversal fills");
    assert_eq!(
        coverage_totals(harness, account, instrument_id),
        (0, 0),
        "no protection guards a reversed position"
    );
    let database =
        rusqlite::Connection::open(&harness.configuration.database_path).expect("group reader");
    let open_groups: i64 = database
        .query_row(
            "SELECT count(*) FROM order_groups WHERE status IN ('pending','active')",
            [],
            |row| row.get(0),
        )
        .expect("open group count");
    assert_eq!(
        open_groups, 0,
        "a reversal through zero completes every group"
    );
    drop(database);
    assert_coverage_invariant(harness, "coverage-walk");
}
/// VAL-TRD-047 seeded runs: brackets, manual orders, quotes that cross protection and
/// restarts in a fixed-seed order; the coverage invariant holds after every step.
#[test]
fn seeded_bracket_coverage_property_holds_after_every_step() {
    for seed in 1..=6 {
        let directory = TestDirectory::new("coverage-property");
        let clock = std::sync::Arc::new(crate::clock::ManualClock::new(5_000));
        let mut harness = start_golden_service(&directory, clock);
        let mut random = XorShift(seed);
        for step in 0..48_usize {
            let value = random.next();
            let sequence = (step * 4) as u64 + 1;
            let time = 10_000 + i64::try_from(step).expect("bounded replay steps");
            match value % 4 {
                0 => {
                    let quantity = i64::try_from(value % 3 + 1).expect("small quantity");
                    let entry = bracket_entry(
                        &format!("prop-entry-{seed}-{step}"),
                        quantity,
                        sequence,
                        time,
                    );
                    let before = harness.snapshot().expect("before bracket");
                    // Capacity bounds and risk locks reject some late-run brackets; a
                    // rejection is a verified no-op, never a partial mutation.
                    if harness
                        .place_inline_bracket(PlaceInlineBracket {
                            entry,
                            template: two_target_template(),
                        })
                        .is_err()
                    {
                        assert_eq!(
                            harness.snapshot().expect("after rejection"),
                            before,
                            "seed={seed} step={step}: a rejected bracket changed state"
                        );
                    }
                }
                1 => {
                    let bid = 9_700 + i64::try_from(value % 500).expect("bounded bid");
                    harness
                        .observe_market(observation(bid, bid + 25, sequence, time))
                        .unwrap_or_else(|error| panic!("seed={seed} step={step}: {error}"));
                }
                2 => {
                    let side = if value & 2 == 0 {
                        OrderSide::Buy
                    } else {
                        OrderSide::Sell
                    };
                    let order =
                        manual_market(&format!("prop-manual-{seed}-{step}"), side, 1, sequence);
                    if harness.place_order(order).is_ok() {
                        let bid = 9_900 + i64::try_from(value % 201).expect("bounded bid");
                        harness
                            .observe_market(observation(bid, bid + 25, sequence + 1, time))
                            .unwrap_or_else(|error| panic!("seed={seed} step={step}: {error}"));
                    }
                }
                _ => harness
                    .restart()
                    .unwrap_or_else(|error| panic!("seed={seed} step={step}: {error}")),
            }
            assert_coverage_invariant(&harness, &format!("seed={seed} step={step}"));
        }
    }
}
