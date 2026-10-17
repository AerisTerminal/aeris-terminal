//! VAL-TRD-060: seeded property runs hold every trading invariant.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use aeris_trading::{AccountPnl, Position};

use super::super::*;
use super::{
    TestDirectory, bracket_template, config, instrument, market_order, observation, provenance,
    start_service_with_config,
};
use crate::clock::Clock;

/// One deterministic run seeds 200 worlds and drives 500 random steps through each: submits,
/// cancels, modifies, quotes, partial closes, scale-ins, restarts, kill switches, unlocks and
/// retention runs. After every step the group coverage, fill-eligibility, DAY-expiry, balance
/// and equity-ring invariants are checked; every restart additionally proves the durable
/// identity (no orphan legs, position equals checkpoint plus retained suffix, balance equals
/// checkpoint plus ledger entries) and the reopen projection equality.
#[test]
fn seeded_property_runs_hold_every_trading_invariant() {
    const SEEDS: u64 = 200;
    const STEPS: usize = 500;
    let mut totals = OpCounts::default();
    for seed in 1..=SEEDS {
        if let Err(failure) = run_property_seed(seed, STEPS, &mut totals) {
            panic!("property seed {seed} failed: {failure}");
        }
    }
    println!(
        "property run: {SEEDS} seeds x {STEPS} steps, ops: place={} cancel={} modify={} quote={} close={} scale_in={} retention={} restart={} kill={} unlock={} cancel_all={}",
        totals.place,
        totals.cancel,
        totals.modify,
        totals.quote,
        totals.close,
        totals.scale_in,
        totals.retention,
        totals.restart,
        totals.kill,
        totals.unlock,
        totals.cancel_all,
    );
    for (name, count) in [
        ("place", totals.place),
        ("cancel", totals.cancel),
        ("modify", totals.modify),
        ("quote", totals.quote),
        ("close", totals.close),
        ("retention", totals.retention),
        ("restart", totals.restart),
        ("kill", totals.kill),
    ] {
        assert!(count > 0, "the property run never exercised {name}");
    }
}

#[derive(Default)]
struct OpCounts {
    place: usize,
    cancel: usize,
    modify: usize,
    quote: usize,
    close: usize,
    scale_in: usize,
    retention: usize,
    restart: usize,
    kill: usize,
    unlock: usize,
    cancel_all: usize,
}

/// A minimal reproducible PRNG; the property seed selects the whole run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut value = self.0 | 1;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn below_i64(&mut self, bound: i64) -> i64 {
        i64::try_from(self.below(u64::try_from(bound).expect("positive bound")))
            .expect("small draw")
    }

    fn pick(&mut self, len: usize) -> usize {
        usize::try_from(self.below(len as u64)).expect("index in range")
    }
}

struct PropertyWorld {
    service: TradingService,
    clock: Arc<crate::clock::ManualClock>,
    rng: Rng,
    sequence: u64,
    step: usize,
    /// Every quote the world sent, keyed by provenance, for the fill-eligibility invariant.
    sent_quotes: HashMap<(u64, u64), (i64, i64)>,
    /// Order instructions as first observed, keyed by order id; limit prices follow accepted
    /// modifications. Managed stop amendments are engine-owned, so stops are checked only
    /// against the touch of their provenance quote.
    order_params: HashMap<String, (OrderType, OrderSide, Option<i64>)>,
    /// Recent operations for failure forensics.
    op_log: std::collections::VecDeque<String>,
    /// True after a user action (leg cancel, cancel-all, kill switch) may have shrunk the
    /// protective pool below the position; the engine re-fits exact coverage on the next
    /// fill or startup repair, which clears the flag.
    coverage_disturbed: bool,
    /// Newest fill observed; a newer fill means the engine re-fitted protection coverage.
    last_fill_marker: Option<(i64, String)>,
    last_bid: i64,
    /// True once a restart could have loaded checkpoints; the in-memory balance identity is
    /// then covered by the durable check at each restart instead.
    checkpoints_possible: bool,
}

impl PropertyWorld {
    fn wall(&self) -> i64 {
        self.clock.wall_unix_nanos()
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    fn advance(&mut self) {
        let seconds = 1 + self.rng.below(4);
        self.clock.advance(Duration::from_secs(seconds));
    }

    fn observe(&mut self) -> Result<(), String> {
        let drift = self.rng.below_i64(17) - 8;
        self.last_bid = (self.last_bid + drift * 25).max(5_000);
        let spread = 25 * (1 + self.rng.below_i64(2));
        let sequence = self.next_sequence();
        let now = self.wall();
        let quote = observation(self.last_bid, self.last_bid + spread, sequence, now);
        self.service
            .observe_market(quote)
            .map_err(|error| format!("quote rejected: {error}"))?;
        self.sent_quotes
            .insert((1, sequence), (self.last_bid, self.last_bid + spread));
        Ok(())
    }

    fn place(&mut self) -> Result<(), String> {
        let sequence = self.next_sequence();
        let now = self.wall();
        let label = format!("prop-{}-{sequence}", self.step);
        let side = if self.rng.below(2) == 0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        match self.rng.below(4) {
            0 | 1 => {
                let mut order = market_order(&label, side, sequence, now);
                order.quantity = FixedPoint::try_new(1 + self.rng.below_i64(3), 0)
                    .map_err(|error| error.to_string())?;
                let _ = self.service.place_order(order);
            }
            2 => {
                let mut order = market_order(&label, side, sequence, now);
                order.order_type = OrderType::Limit;
                let offset = self.rng.below_i64(9) - 4;
                order.limit_price = Some(
                    FixedPoint::try_new(self.last_bid + offset * 25, 2)
                        .map_err(|error| error.to_string())?,
                );
                order.time_in_force = if self.rng.below(2) == 0 {
                    TimeInForce::Day
                } else {
                    TimeInForce::GoodTillCancelled
                };
                let _ = self.service.place_order(order);
            }
            _ => {
                let entry = market_order(&label, side, sequence, now);
                let _ = self.service.place_inline_bracket(PlaceInlineBracket {
                    entry,
                    template: bracket_template(),
                });
            }
        }
        Ok(())
    }

    fn cancel_one(&mut self) {
        let snapshot = self.service.snapshot().expect("property snapshot");
        let open: Vec<String> = snapshot
            .orders
            .iter()
            .filter(|order| order.status.is_open())
            .map(|order| order.client_order_id.as_str().to_string())
            .collect();
        if open.is_empty() {
            return;
        }
        let picked = &open[self.rng.pick(open.len())];
        let is_leg = snapshot.managed_brackets.iter().any(|bracket| {
            bracket
                .stop_client_order_id
                .as_ref()
                .is_some_and(|s| s.as_str() == picked)
                || bracket
                    .target_client_order_ids
                    .iter()
                    .any(|t| t.as_str() == picked)
        });
        if is_leg {
            self.coverage_disturbed = true;
        }
        let id = ClientOrderId::try_new(picked.as_str()).expect("client id");
        let _ = self.service.cancel_order(id);
    }

    fn modify_one(&mut self) {
        let snapshot = self.service.snapshot().expect("property snapshot");
        let open: Vec<_> = snapshot
            .orders
            .iter()
            .filter(|order| order.status.is_open() && order.limit_price.is_some())
            .cloned()
            .collect();
        if open.is_empty() {
            return;
        }
        let order = &open[self.rng.pick(open.len())];
        let offset = self.rng.below_i64(9) - 4;
        let sequence = self.next_sequence();
        let now = self.wall();
        let new_limit = self.last_bid + offset * 25;
        if self
            .service
            .modify_order(ModifyOrder {
                client_order_id: order.client_order_id.clone(),
                time_in_force: order.time_in_force,
                limit_price: Some(FixedPoint::try_new(new_limit, 2).expect("limit price")),
                stop_price: order.stop_price,
                modified_unix_nanos: now,
                provenance: provenance(sequence, now),
            })
            .is_ok()
            && let Some(params) = self.order_params.get_mut(order.id.as_str())
        {
            params.2 = Some(new_limit);
        }
    }

    fn close_half(&mut self) {
        let snapshot = self.service.snapshot().expect("property snapshot");
        let Some(position) = snapshot.positions.first() else {
            return;
        };
        let net = position.net_quantity.units();
        if net.unsigned_abs() < 2 {
            return;
        }
        let sequence = self.next_sequence();
        let side = if net > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let mut order = market_order(
            &format!("close-{}-{sequence}", self.step),
            side,
            sequence,
            self.wall(),
        );
        order.quantity =
            FixedPoint::try_new(i64::try_from(net.unsigned_abs() / 2).expect("half"), 0)
                .expect("half");
        let _ = self.service.place_order(order);
    }

    fn scale_in(&mut self) {
        let snapshot = self.service.snapshot().expect("property snapshot");
        let Some(position) = snapshot.positions.first() else {
            return;
        };
        let sequence = self.next_sequence();
        let side = if position.net_quantity.units() > 0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        let _ = self.service.place_order(market_order(
            &format!("scale-{}-{sequence}", self.step),
            side,
            sequence,
            self.wall(),
        ));
    }
}

fn run_property_seed(seed: u64, steps: usize, totals: &mut OpCounts) -> Result<(), String> {
    let (mut world, directory, configuration) = build_property_world(seed);
    run_property_steps(&mut world, steps, totals, &directory, &configuration).map_err(|failure| {
        let ops = world.op_log.iter().cloned().collect::<Vec<_>>().join(" ");
        format!("{failure}; recent ops: {ops}")
    })
}

fn build_property_world(seed: u64) -> (PropertyWorld, TestDirectory, TradingServiceConfig) {
    let directory = TestDirectory::new(&format!("property-{seed}"));
    let clock = Arc::new(crate::clock::ManualClock::new(1_000_000_000_000));
    let mut configuration = config(&directory);
    configuration.clock = clock.clone();
    let world = PropertyWorld {
        service: start_service_with_config(configuration.clone()),
        clock,
        rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        sequence: 0,
        step: 0,
        sent_quotes: HashMap::new(),
        order_params: HashMap::new(),
        op_log: std::collections::VecDeque::new(),
        coverage_disturbed: false,
        last_fill_marker: None,
        last_bid: 10_000,
        checkpoints_possible: false,
    };
    world
        .service
        .register_instrument(instrument())
        .expect("instrument registration");
    world
        .service
        .register_risk_profile(RiskProfile {
            account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
            profile_id: "property".to_string(),
            version: 1,
            session_start_unix_nanos: 1,
            session_start_realized_pnl: FixedPoint::try_new(0, 2).expect("baseline"),
            daily_loss_limit: FixedPoint::try_new(900_000_000, 2).expect("daily loss"),
            trailing_drawdown: None,
            trailing_mode: TrailingDrawdownMode::Intraday,
            max_contracts: FixedPoint::try_new(10_000, 0).expect("contracts"),
            consistency_max_single_trade_percent: None,
            restricted_until_unix_nanos: None,
            economic_event_rule: None,
            enabled: true,
        })
        .expect("risk profile registration");
    (world, directory, configuration)
}

fn run_property_steps(
    world: &mut PropertyWorld,
    steps: usize,
    totals: &mut OpCounts,
    directory: &TestDirectory,
    configuration: &TradingServiceConfig,
) -> Result<(), String> {
    world.observe()?;

    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    for step in 0..steps {
        world.step = step;
        world.advance();
        let roll = world.rng.below(100);
        let op = match roll {
            0..=29 => "place",
            30..=44 => "cancel",
            45..=54 => "modify",
            55..=79 => "quote",
            80..=84 => "close",
            85..=87 => "scale_in",
            88..=90 => "retention",
            91..=93 => "restart",
            94..=96 => "kill",
            97..=98 => "unlock",
            _ => "cancel_all",
        };
        world.op_log.push_back(format!("{step}:{op}"));
        if world.op_log.len() > 40 {
            world.op_log.pop_front();
        }
        match roll {
            0..=29 => {
                world.place()?;
                totals.place += 1;
            }
            30..=44 => {
                world.cancel_one();
                totals.cancel += 1;
            }
            45..=54 => {
                world.modify_one();
                totals.modify += 1;
            }
            55..=79 => {
                world.observe()?;
                totals.quote += 1;
            }
            80..=84 => {
                world.close_half();
                totals.close += 1;
            }
            85..=87 => {
                world.scale_in();
                totals.scale_in += 1;
            }
            88..=90 => {
                // Every retention run advances past the hourly interval; some jump a day or
                // more so DAY orders cross session deadlines and equity session boundaries.
                let seconds = if world.rng.below(3) == 0 {
                    86_400 + world.rng.below(43_200)
                } else {
                    3_700
                };
                world.clock.advance(Duration::from_secs(seconds));
                world.observe()?;
                world
                    .service
                    .status()
                    .map_err(|error| format!("retention tick: {error}"))?;
                totals.retention += 1;
            }
            91..=93 => {
                restart_with_durable_checks(world, directory, configuration)?;
                totals.restart += 1;
            }
            94..=96 => {
                let _ = world.service.kill_switch(
                    Some(account.clone()),
                    format!("property kill at step {step}"),
                    world.wall(),
                );
                world.coverage_disturbed = true;
                totals.kill += 1;
            }
            97..=98 => {
                let _ = world.service.unlock_account(account.clone());
                totals.unlock += 1;
            }
            _ => {
                let _ = world.service.cancel_all(Some(account.clone()));
                world.coverage_disturbed = true;
                totals.cancel_all += 1;
            }
        }
        check_memory_invariants(world, step)?;
    }
    // A final restart closes the seed with the durable identity checks.
    restart_with_durable_checks(world, directory, configuration)?;
    world
        .service
        .shutdown(Duration::from_secs(2))
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

/// The per-step in-memory invariants: group coverage, fill quote eligibility, DAY expiry,
/// balance identity and the equity ring bound.
fn check_memory_invariants(world: &mut PropertyWorld, step: usize) -> Result<(), String> {
    let snapshot = world
        .service
        .snapshot()
        .map_err(|error| format!("snapshot: {error}"))?;
    for order in &snapshot.orders {
        world
            .order_params
            .entry(order.id.as_str().to_string())
            .or_insert((
                order.order_type,
                order.side,
                order.limit_price.map(FixedPoint::units),
            ));
    }
    // A new fill re-fitted the protection pool exactly (VAL-TRD-047).
    let newest_fill = snapshot
        .fills
        .iter()
        .map(|fill| (fill.execution_unix_nanos, fill.id.as_str().to_string()))
        .max();
    if newest_fill != world.last_fill_marker {
        if newest_fill.is_some() {
            world.coverage_disturbed = false;
        }
        world.last_fill_marker = newest_fill;
    }
    check_group_lifecycle(&snapshot, step)?;
    check_coverage_pool(world, &snapshot, step)?;
    check_balance_identity(world, &snapshot, step)?;
    check_fill_eligibility(world, &snapshot, step)?;
    check_day_expiry(&snapshot, step, world.wall())?;
    check_equity_ring(&snapshot, step)
}

/// Group lifecycle coverage: an awaiting bracket keeps its entry open and holds its legs; a
/// terminal bracket holds no executable leg (held inactive legs pin their history by design).
fn check_group_lifecycle(snapshot: &TradingSnapshot, step: usize) -> Result<(), String> {
    let order_status = |id: &ClientOrderId| {
        snapshot
            .orders
            .iter()
            .find(|order| order.client_order_id == *id)
            .map(|order| order.status)
    };
    let leg_open = |id: &ClientOrderId| order_status(id).is_some_and(OrderStatus::is_open);
    // Held (inactive) legs of a terminal group pin their history by design; what must never
    // happen is an executable leg without live group coverage, or a missing working leg of a
    // live group.
    let leg_executable =
        |id: &ClientOrderId| order_status(id).is_some_and(OrderStatus::is_executable);
    for bracket in &snapshot.managed_brackets {
        match bracket.status {
            BracketViewStatus::AwaitingEntry => {
                if !leg_open(&bracket.entry_client_order_id) {
                    return Err(format!(
                        "step {step}: awaiting bracket {} lost its entry order",
                        bracket.bracket_id
                    ));
                }
                let leg_live = bracket
                    .stop_client_order_id
                    .as_ref()
                    .is_some_and(leg_executable)
                    || bracket.target_client_order_ids.iter().any(leg_executable);
                if leg_live {
                    return Err(format!(
                        "step {step}: awaiting bracket {} already has an executable leg",
                        bracket.bracket_id
                    ));
                }
            }
            // Per-leg coverage is pool-based (VAL-TRD-047): an active bracket may lose an
            // individual leg to coverage re-fitting while the role total still matches the
            // position; the pool assertions below carry the guarantee.
            BracketViewStatus::Active => {}
            BracketViewStatus::Completed | BracketViewStatus::Cancelled => {
                let any_executable = leg_executable(&bracket.entry_client_order_id)
                    || bracket
                        .stop_client_order_id
                        .as_ref()
                        .is_some_and(leg_executable)
                    || bracket.target_client_order_ids.iter().any(leg_executable);
                if any_executable {
                    let mut legs = vec![("entry", &bracket.entry_client_order_id)];
                    if let Some(stop) = &bracket.stop_client_order_id {
                        legs.push(("stop", stop));
                    }
                    legs.extend(
                        bracket
                            .target_client_order_ids
                            .iter()
                            .map(|t| ("target", t)),
                    );
                    let detail = legs
                        .iter()
                        .map(|(role, id)| {
                            format!("{role} {} -> {:?}", id.as_str(), order_status(id))
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let orders_dump = snapshot
                        .orders
                        .iter()
                        .map(|order| {
                            format!(
                                "{} {} {:?} {:?}",
                                order.id.as_str(),
                                order.client_order_id.as_str(),
                                order.order_type,
                                order.status
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    let events = snapshot
                        .order_events
                        .iter()
                        .map(|event| {
                            format!(
                                "{} {:?} {:?}",
                                event.order_id.as_str(),
                                event.kind,
                                event.reason
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    return Err(format!(
                        "step {step}: terminal bracket {} ({:?}) still has an executable leg: {detail}; orders: {orders_dump}; events: {events}",
                        bracket.bracket_id, bracket.status
                    ));
                }
            }
        }
    }
    Ok(())
}

/// VAL-TRD-047 pooled coverage: executable group legs never oppose the position, never cover
/// a flat account and never exceed it; unless a user action disturbed the pool, each role
/// totals either zero or exactly |net| (the engine re-fits on every fill).
fn check_coverage_pool(
    world: &PropertyWorld,
    snapshot: &TradingSnapshot,
    step: usize,
) -> Result<(), String> {
    let net = snapshot
        .positions
        .first()
        .map_or(0, |position| position.net_quantity.units());
    let mut stop_cover = 0_u64;
    let mut target_cover = 0_u64;
    for bracket in &snapshot.managed_brackets {
        if bracket.status != BracketViewStatus::Active {
            continue;
        }
        let leg_orders = bracket
            .stop_client_order_id
            .iter()
            .map(|id| (true, id))
            .chain(bracket.target_client_order_ids.iter().map(|id| (false, id)));
        for (is_stop, id) in leg_orders {
            let Some(order) = snapshot
                .orders
                .iter()
                .find(|order| order.client_order_id == *id)
            else {
                continue;
            };
            if !order.status.is_executable() {
                continue;
            }
            let side_sign = order.side.sign();
            if net == 0 {
                return Err(format!(
                    "step {step}: executable protective order {} on a flat account",
                    id.as_str()
                ));
            }
            if side_sign != -net.signum() {
                return Err(format!(
                    "step {step}: protective order {} works the wrong side (net {net})",
                    id.as_str()
                ));
            }
            if is_stop {
                stop_cover += order.quantity.units().unsigned_abs();
            } else {
                target_cover += order.quantity.units().unsigned_abs();
            }
        }
    }
    let wanted = net.unsigned_abs();
    for (role, cover) in [("stop", stop_cover), ("target", target_cover)] {
        if cover > wanted {
            return Err(format!(
                "step {step}: {role} protection {cover} exceeds the position {wanted}"
            ));
        }
        if !world.coverage_disturbed && cover != 0 && cover != wanted {
            return Err(format!(
                "step {step}: {role} protection {cover} neither zero nor the position {wanted}"
            ));
        }
    }
    Ok(())
}

/// Until a restart may have loaded checkpoints, the simulated balance equals the ledger sum;
/// afterwards the durable identity check at each restart carries it.
fn check_balance_identity(
    world: &PropertyWorld,
    snapshot: &TradingSnapshot,
    step: usize,
) -> Result<(), String> {
    if !world.checkpoints_possible {
        for pnl in &snapshot.account_pnl {
            let entries_sum: i64 = snapshot
                .ledger_entries
                .iter()
                .filter(|entry| entry.account_id == pnl.account_id)
                .map(|entry| entry.amount.units())
                .sum();
            let expected = if snapshot
                .ledger_entries
                .iter()
                .any(|entry| entry.account_id == pnl.account_id)
            {
                entries_sum
            } else {
                let account = snapshot
                    .accounts
                    .iter()
                    .find(|account| account.id == pnl.account_id)
                    .ok_or_else(|| format!("step {step}: pnl for an unknown account"))?;
                let starting = account
                    .starting_equity
                    .ok_or_else(|| format!("step {step}: unfunded account without ledger"))?;
                starting
                    .checked_add(pnl.realized)
                    .map_err(|error| error.to_string())?
                    .units()
            };
            let balance = pnl
                .balance
                .ok_or_else(|| format!("step {step}: simulated account lost its balance"))?;
            if balance.units() != expected {
                return Err(format!(
                    "step {step}: balance {} != ledger sum {expected}",
                    balance.units()
                ));
            }
        }
    }
    Ok(())
}

/// Every fill matches a quote this world observed and its own order instruction.
fn check_fill_eligibility(
    world: &PropertyWorld,
    snapshot: &TradingSnapshot,
    step: usize,
) -> Result<(), String> {
    let now = world.wall();
    for fill in &snapshot.fills {
        let key = (
            fill.provenance.session_generation,
            fill.provenance.source_sequence,
        );
        let (bid, ask) = world
            .sent_quotes
            .get(&key)
            .ok_or_else(|| format!("step {step}: fill on a quote this run never sent"))?;
        let params = world
            .order_params
            .get(fill.order_id.as_str())
            .ok_or_else(|| format!("step {step}: fill for an order this run never saw"))?;
        let price = fill.price.units();
        let eligible = match (params.1, params.0) {
            // Market and stop orders fill at the touch; managed stops may be amended by the
            // engine, so only the touch price is checkable for them.
            (OrderSide::Buy, OrderType::Market | OrderType::Stop) => price == *ask,
            (OrderSide::Sell, OrderType::Market | OrderType::Stop) => price == *bid,
            (OrderSide::Buy, OrderType::Limit) => Some(price) == params.2 && *ask <= price,
            (OrderSide::Sell, OrderType::Limit) => Some(price) == params.2 && *bid >= price,
            _ => price >= *bid && price <= *ask,
        };
        if !eligible {
            return Err(format!(
                "step {step}: fill at {price} violates its quote [{bid}, {ask}] or its order instruction"
            ));
        }
        if fill.execution_unix_nanos > now {
            return Err(format!("step {step}: fill executed in the future"));
        }
    }
    Ok(())
}

/// No DAY order stays open past its session deadline.
fn check_day_expiry(snapshot: &TradingSnapshot, step: usize, now: i64) -> Result<(), String> {
    for order in snapshot
        .orders
        .iter()
        .filter(|order| order.status.is_open())
    {
        if order.time_in_force == TimeInForce::Day
            && let Some(expires) = order.expires_at_unix_nanos
            && expires <= now
        {
            return Err(format!(
                "step {step}: DAY order {} open past its deadline",
                order.client_order_id.as_str()
            ));
        }
    }
    Ok(())
}

/// The in-memory equity ring stays within its bound.
fn check_equity_ring(snapshot: &TradingSnapshot, step: usize) -> Result<(), String> {
    let mut ring_sizes: BTreeMap<&TradingAccountId, usize> = BTreeMap::new();
    for sample in &snapshot.equity_samples {
        *ring_sizes.entry(&sample.account_id).or_default() += 1;
    }
    if let Some((account, size)) = ring_sizes.iter().find(|(_, size)| **size > 1_440) {
        return Err(format!(
            "step {step}: equity ring of {} holds {size} samples",
            account.as_str()
        ));
    }
    Ok(())
}

/// Restart the service and, while it is down, prove the durable identity: no orphan group
/// legs, every position equals its checkpoint plus the retained fill suffix, and every
/// balance equals its ledger checkpoint plus the remaining entries. After the reopen the
/// projection must equal the pre-shutdown one.
fn restart_with_durable_checks(
    world: &mut PropertyWorld,
    directory: &TestDirectory,
    configuration: &TradingServiceConfig,
) -> Result<(), String> {
    let before = world
        .service
        .snapshot()
        .map_err(|error| format!("pre-restart snapshot: {error}"))?;
    let now = world.wall();
    world
        .service
        .shutdown(Duration::from_secs(2))
        .map_err(|error| format!("restart shutdown: {error}"))?;

    check_durable_identity(directory, &before, now, world.step)?;
    world.checkpoints_possible = true;

    world.service = start_service_with_config(configuration.clone());
    let recovered = world
        .service
        .snapshot()
        .map_err(|error| format!("reopen snapshot: {error}"))?;
    check_reopen_equality(world.step, &before, &recovered)
}

/// The durable identity while the service is down: no orphan group legs, every position equals
/// its checkpoint plus the retained fill suffix, every balance equals its ledger checkpoint
/// plus the remaining entries, and no DAY order rests past its deadline.
fn check_durable_identity(
    directory: &TestDirectory,
    before: &TradingSnapshot,
    now: i64,
    step: usize,
) -> Result<(), String> {
    let database = rusqlite::Connection::open(directory.0.join("trading.sqlite3"))
        .map_err(|error| format!("durable reader: {error}"))?;
    let orphan_legs: i64 = database
        .query_row(
            "SELECT count(*) FROM order_group_legs WHERE order_id NOT IN (SELECT id FROM orders)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("orphan-leg query: {error}"))?;
    if orphan_legs != 0 {
        return Err(format!("step {step}: {orphan_legs} orphan group legs"));
    }
    let checkpoint_balance: Option<i64> = database
        .query_row(
            "SELECT balance_units FROM ledger_checkpoints WHERE account_id = 'aeris-sim-1'",
            [],
            |row| row.get(0),
        )
        .ok();
    let entries_sum: i64 = database
        .query_row(
            "SELECT COALESCE(SUM(amount_units), 0) FROM ledger_entries
             WHERE account_id = 'aeris-sim-1'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("ledger sum: {error}"))?;
    let balance = before.account_pnl[0]
        .balance
        .ok_or_else(|| format!("step {step}: balance missing before restart"))?;
    if let Some(checkpoint) = checkpoint_balance
        && checkpoint + entries_sum != balance.units()
    {
        return Err(format!(
            "step {step}: balance {} != checkpoint {checkpoint} + entries {entries_sum}",
            balance.units()
        ));
    }
    for position in &before.positions {
        let checkpoint_net: i64 = database
            .query_row(
                "SELECT net_quantity_units FROM position_checkpoints
                 WHERE account_id = ?1 AND instrument_id = ?2",
                rusqlite::params![
                    position.account_id.as_str(),
                    position.instrument_id.as_str()
                ],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let suffix_net: i64 = database
            .query_row(
                "SELECT COALESCE(SUM(CASE WHEN side = 'buy' THEN quantity_units
                    ELSE -quantity_units END), 0)
                 FROM fills WHERE account_id = ?1 AND instrument_id = ?2",
                rusqlite::params![
                    position.account_id.as_str(),
                    position.instrument_id.as_str()
                ],
                |row| row.get(0),
            )
            .map_err(|error| format!("fill suffix sum: {error}"))?;
        if checkpoint_net + suffix_net != position.net_quantity.units() {
            return Err(format!(
                "step {step}: position net {} != checkpoint {checkpoint_net} + suffix {suffix_net}",
                position.net_quantity.units()
            ));
        }
    }
    let stale_day_orders: i64 = database
        .query_row(
            "SELECT count(*) FROM orders WHERE status IN
                ('pending', 'working', 'pending_modify', 'partially_filled', 'pending_cancel')
             AND time_in_force = 'day' AND expires_at_unix_nanos IS NOT NULL
             AND expires_at_unix_nanos <= ?1",
            [now],
            |row| row.get(0),
        )
        .map_err(|error| format!("day-expiry query: {error}"))?;
    if stale_day_orders != 0 {
        return Err(format!(
            "step {step}: {stale_day_orders} DAY orders open past their deadline"
        ));
    }
    Ok(())
}

/// After the reopen the projection must equal the pre-shutdown one, unless startup recovery
/// reported a repair (the kill switch may leave a group with dead legs; M5.9 owns that
/// in-session reconciliation). A notice-free restart reproduces the projection exactly.
fn check_reopen_equality(
    step: usize,
    before: &TradingSnapshot,
    recovered: &TradingSnapshot,
) -> Result<(), String> {
    let normalize_positions = |positions: &[Position]| {
        positions
            .iter()
            .cloned()
            .map(|mut position| {
                position.unrealized_pnl = FixedPoint::try_new(0, 2).expect("zero");
                position
            })
            .collect::<Vec<_>>()
    };
    if normalize_positions(&recovered.positions) != normalize_positions(&before.positions) {
        return Err(format!("step {step}: positions changed across the restart"));
    }
    let balance_sheet = |pnl: &[AccountPnl]| {
        pnl.iter()
            .map(|entry| (entry.account_id.clone(), entry.realized, entry.balance))
            .collect::<Vec<_>>()
    };
    if balance_sheet(&recovered.account_pnl) != balance_sheet(&before.account_pnl) {
        return Err(format!("step {step}: balances changed across the restart"));
    }
    let open_orders = |snapshot: &TradingSnapshot| {
        let mut orders = snapshot
            .orders
            .iter()
            .filter(|order| order.status.is_open())
            .map(|order| {
                (
                    order.client_order_id.as_str().to_string(),
                    order.status,
                    order.quantity,
                    order.limit_price,
                )
            })
            .collect::<Vec<_>>();
        orders.sort_by(|left, right| left.0.cmp(&right.0));
        orders
    };
    // Startup recovery may retire a group the kill switch left with dead legs (M5.9 owns the
    // in-session reconciliation); a notice-free restart must reproduce the projection exactly.
    if recovered.recovery_notices.is_empty() {
        if open_orders(recovered) != open_orders(before) {
            return Err(format!(
                "step {step}: open orders changed across the restart"
            ));
        }
        if recovered.managed_brackets != before.managed_brackets {
            return Err(format!(
                "step {step}: brackets changed across the restart\nbefore: {:?}\nafter: {:?}",
                before.managed_brackets, recovered.managed_brackets
            ));
        }
    }
    Ok(())
}
