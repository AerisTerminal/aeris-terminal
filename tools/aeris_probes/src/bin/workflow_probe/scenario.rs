//! The workflow scenario: save and arm a price workflow, let the live market cross it, let
//! a time-of-day workflow close the position, restart the trading owner and check that
//! nothing fires twice, then delete both workflows.

use std::{
    path::Path,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use aeris_instruments::InstrumentId;
use aeris_market_runtime::MarketService;
use aeris_platform_runtime::data_root::{DataRoot, InstanceLock};
use aeris_probes::{
    Evidence, ProbeError, ProbeResult,
    feed::{FeedTarget, QuoteRecorder, RecordedQuote},
    market, unix_nanos_now, wait_for,
};
use aeris_trading::{
    AccountEnvironment, ClientOrderId, FixedPoint, Order, OrderSide, OrderStatus, TradingAccount,
    market::MarketRoute,
};
use aeris_trading_runtime::{
    BracketViewStatus, CreatePracticeAccount, ExecutionPolicy, HistoryQuery, PositionRequirement,
    PriceDirection, TradingRetention, TradingService, TradingServiceConfig, WorkflowAction,
    WorkflowActionResult, WorkflowClockTime, WorkflowCondition, WorkflowDefinition, WorkflowError,
    WorkflowFiringOutcome, WorkflowLayout, WorkflowLimits, WorkflowNode, WorkflowNodeId,
    WorkflowNodePosition, WorkflowProtection, WorkflowRepeat, WorkflowState, WorkflowStep,
    WorkflowStepOutcome, WorkflowTimeZone, WorkflowTrigger, WorkflowView, WorkingSet,
    clock::SystemClock,
};
use serde_json::json;

const PROVIDER: &str = "hyperliquid";
const BTC_ID: &str = "hyperliquid:perp:BTC";
const ACCOUNT_NAME: &str = "Workflow Probe";
const STARTING_EQUITY: (i64, u8) = (10_000_000, 2); // $100,000.00
const QUANTITY: (i64, u8) = (1_000_000, 8); // 0.01 BTC
const PROTECTION_TICKS: u32 = 500;
const BREAKOUT_ID: &str = "probe-breakout";
const CLOSE_ID: &str = "probe-timed-close";
/// Each attempt levels the trigger at the current ask; a market that only falls during one
/// attempt is retried with a new revision at the new ask.
const CROSS_ATTEMPTS: u32 = 5;
const CROSS_WAIT: Duration = Duration::from_secs(60);
const QUOTE_WAIT: Duration = Duration::from_secs(30);
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const NANOS_PER_MINUTE: i64 = 60 * NANOS_PER_SECOND;
const MINUTES_PER_DAY: i64 = 1_440;
/// The owner timer wakes at the due minute; this bounds how late a time trigger may fire.
const TIME_TRIGGER_TOLERANCE: i64 = 5 * NANOS_PER_SECOND;

fn fixed(value: (i64, u8)) -> ProbeResult<FixedPoint> {
    FixedPoint::try_new(value.0, value.1).map_err(|error| ProbeError::Runtime(error.to_string()))
}

fn owner<T>(result: Result<T, WorkflowError>) -> ProbeResult<T> {
    result.map_err(|error| ProbeError::Runtime(error.to_string()))
}

/// Polls like `wait_for`, but a deadline is an expected outcome rather than a failure.
fn poll_until<T>(
    timeout: Duration,
    mut poll: impl FnMut() -> ProbeResult<Option<T>>,
) -> ProbeResult<Option<T>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = poll()? {
            return Ok(Some(value));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// The probe declares the durable route identity; verified terms arrive from the coordinator.
fn ticket_route() -> ProbeResult<MarketRoute> {
    Ok(MarketRoute {
        provider_id: PROVIDER.to_string(),
        instrument_id: InstrumentId::try_new(BTC_ID)
            .map_err(|error| ProbeError::Runtime(error.to_string()))?,
        provider_symbol: "BTC".to_string(),
        display_symbol: "BTC-USDC".to_string(),
        venue_id: "Hyperliquid".to_string(),
        entitlement_id: Some("hyperliquid-public".to_string()),
        price_scale: 8,
        quantity_scale: 8,
    })
}

fn start_trading(root: &Path) -> ProbeResult<TradingService> {
    let directory = root.join("trading");
    std::fs::create_dir_all(&directory)?;
    TradingService::start(TradingServiceConfig {
        database_path: directory.join("trading.sqlite3"),
        retention: TradingRetention::default(),
        clock: Arc::new(SystemClock::default()),
        execution: ExecutionPolicy::default(),
    })
    .map_err(|error| ProbeError::Runtime(error.to_string()))
}

/// The owner-published working set of a fresh subscription, so no stale event is read.
fn working_set(trading: &TradingService) -> ProbeResult<WorkingSet> {
    let subscription = market(trading.subscribe())?;
    subscription
        .recv_timeout(Duration::from_secs(5))
        .map(|update| *update.event.working_set)
        .ok_or_else(|| ProbeError::Runtime("initial trading publication timed out".to_string()))
}

fn order_history(trading: &TradingService) -> ProbeResult<Vec<Order>> {
    let mut orders = Vec::new();
    let mut after = None;
    loop {
        let page = market(trading.history(HistoryQuery {
            after,
            ..HistoryQuery::default()
        }))?;
        orders.extend(page.orders);
        match page.next {
            Some(cursor) => after = Some(cursor),
            None => return Ok(orders),
        }
    }
}

fn find_order(
    trading: &TradingService,
    client_order_id: &ClientOrderId,
) -> ProbeResult<Option<Order>> {
    Ok(order_history(trading)?
        .into_iter()
        .find(|order| &order.client_order_id == client_order_id))
}

/// Bounded shutdown of the active trading owner on every exit path.
struct TradingGuard(Option<TradingService>);

impl TradingGuard {
    fn current(&self) -> ProbeResult<TradingService> {
        self.0
            .clone()
            .ok_or_else(|| ProbeError::Invariant("trading service is not running".to_string()))
    }
}

impl Drop for TradingGuard {
    fn drop(&mut self) {
        if let Some(service) = self.0.take()
            && let Err(error) = service.shutdown(Duration::from_secs(2))
        {
            eprintln!("trading owner shutdown failed: {error}");
        }
    }
}

/// Runs the scenario inside one acquired validation root and market service.
///
/// # Errors
/// Returns the first failed check or runtime error; the envelope still records evidence.
pub fn run(root: &DataRoot, evidence: &mut Evidence) -> ProbeResult<()> {
    let _lock =
        InstanceLock::acquire(root).map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let market_service = market(MarketService::start())?;
    let result = run_with_market(&market_service, root, evidence);
    let shutdown = market_service.shutdown(Duration::from_secs(10));
    result?;
    market(shutdown)
}

fn run_with_market(
    market_service: &MarketService,
    root: &DataRoot,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let recorder = Arc::new(QuoteRecorder::new());
    let target = Arc::new(FeedTarget::default());
    aeris_probes::feed::attach_trading_feed(
        market_service,
        Arc::clone(&recorder),
        Arc::clone(&target),
    )?;
    let mut guard = TradingGuard(Some(start_trading(root.path())?));
    let trading = guard.current()?;
    aeris_probes::feed::wire_trading_feed(market_service, &trading, &target)?;
    let route = ticket_route()?;
    market(trading.set_ticket_interest(vec![route.clone()]))?;
    let generation = wait_for(
        "hyperliquid provider session",
        Duration::from_secs(60),
        || {
            let status = market(market_service.status())?;
            Ok(status
                .providers
                .iter()
                .find(|provider| provider.provider == PROVIDER)
                .map(|provider| provider.generation)
                .filter(|generation| *generation > 0))
        },
    )?;
    let terms = wait_for("verified BTC terms", Duration::from_secs(45), || {
        Ok(working_set(&trading)?
            .instruments
            .into_iter()
            .find(|instrument| instrument.instrument_id == route.instrument_id))
    })?;
    wait_for("live BTC quote", QUOTE_WAIT, || {
        Ok(recorder.latest_quote(BTC_ID))
    })?;
    let account = practice_account(&trading)?;
    evidence.check(
        "market_and_account_ready",
        terms.price_scale == route.price_scale,
        json!({
            "provider_generation": generation,
            "price_scale": terms.price_scale,
            "quantity_scale": terms.quantity_scale,
            "account_id": account.id.as_str(),
        }),
    )?;
    let mut probe = Probe {
        trading,
        account,
        instrument: route.instrument_id.clone(),
        recorder: &recorder,
        evidence,
    };
    probe.refuses_an_invalid_workflow()?;
    let entry = probe.breakout_fires_on_a_live_cross()?;
    probe.time_of_day_closes_the_position(&entry)?;
    probe.restart_fires_nothing_again(market_service, &mut guard, &target, &route, root)?;
    probe.deleted_workflows_are_gone()?;
    let (quotes, push_failures) = recorder.statistics();
    probe.evidence.metrics = json!({
        "quotes_recorded": quotes,
        "feed_push_failures": push_failures,
    });
    let trading = guard
        .0
        .take()
        .ok_or_else(|| ProbeError::Invariant("trading service is not running".to_string()))?;
    market(trading.shutdown(Duration::from_secs(5)))
}

/// Creates or reuses the probe's practice account by display name.
fn practice_account(trading: &TradingService) -> ProbeResult<TradingAccount> {
    let existing = working_set(trading)?.accounts.into_iter().find(|account| {
        account.display_name == ACCOUNT_NAME && account.environment == AccountEnvironment::Simulated
    });
    match existing {
        Some(account) => Ok(account),
        None => market(trading.create_practice_account(CreatePracticeAccount {
            display_name: ACCOUNT_NAME.to_string(),
            starting_equity: fixed(STARTING_EQUITY)?,
        })),
    }
}

/// The entry a workflow placed.
struct Entry {
    client_order_id: String,
}

struct Probe<'a> {
    trading: TradingService,
    account: TradingAccount,
    instrument: InstrumentId,
    recorder: &'a QuoteRecorder,
    evidence: &'a mut Evidence,
}

impl Probe<'_> {
    fn current(&self) -> ProbeResult<WorkingSet> {
        working_set(&self.trading)
    }

    fn workflow(&self, workflow_id: &str) -> ProbeResult<Option<WorkflowView>> {
        Ok(self
            .current()?
            .workflows
            .into_iter()
            .find(|view| view.definition.workflow_id == workflow_id))
    }

    fn latest_quote(&self) -> ProbeResult<RecordedQuote> {
        wait_for("fresh BTC quote", QUOTE_WAIT, || {
            Ok(self.recorder.latest_quote(BTC_ID))
        })
    }

    fn position_units(&self) -> ProbeResult<i64> {
        Ok(self
            .current()?
            .positions
            .iter()
            .find(|position| {
                position.account_id == self.account.id && position.instrument_id == self.instrument
            })
            .map_or(0, |position| position.net_quantity.units()))
    }

    /// The trigger, then an If on `condition` whose yes branch runs `actions` in order and
    /// whose no branch ends the run, as the board builds it.
    fn definition(
        &self,
        workflow_id: &str,
        revision: u32,
        name: &str,
        trigger: WorkflowTrigger,
        condition: WorkflowCondition,
        actions: &[WorkflowAction],
    ) -> ProbeResult<WorkflowDefinition> {
        let node = |index: usize| {
            u16::try_from(index + 1)
                .map(WorkflowNodeId)
                .map_err(|error| ProbeError::Runtime(error.to_string()))
        };
        let mut nodes = vec![WorkflowNode {
            id: node(0)?,
            step: WorkflowStep::If {
                condition,
                then: Some(node(1)?),
                otherwise: None,
            },
        }];
        for (index, action) in actions.iter().enumerate() {
            nodes.push(WorkflowNode {
                id: node(index + 1)?,
                step: WorkflowStep::Action {
                    action: *action,
                    next: (index + 1 < actions.len())
                        .then(|| node(index + 2))
                        .transpose()?,
                },
            });
        }
        Ok(WorkflowDefinition {
            workflow_id: workflow_id.to_string(),
            revision,
            name: name.to_string(),
            account_id: self.account.id.clone(),
            instrument_id: self.instrument.clone(),
            trigger,
            start: node(0)?,
            nodes,
            limits: WorkflowLimits {
                repeat: WorkflowRepeat::Once,
                cooldown_seconds: 0,
                expires_unix_nanos: None,
            },
        })
    }

    fn breakout(&self, revision: u32, level: FixedPoint) -> ProbeResult<WorkflowDefinition> {
        self.definition(
            BREAKOUT_ID,
            revision,
            "Probe breakout long",
            WorkflowTrigger::PriceCrosses {
                direction: PriceDirection::Above,
                level,
            },
            WorkflowCondition::Position(PositionRequirement::Flat),
            &[WorkflowAction::Enter {
                side: OrderSide::Buy,
                quantity: fixed(QUANTITY)?,
                protection: Some(WorkflowProtection {
                    stop_ticks: PROTECTION_TICKS,
                    target_ticks: PROTECTION_TICKS,
                }),
            }],
        )
    }

    /// A workflow outside its closed set is refused and never stored.
    fn refuses_an_invalid_workflow(&mut self) -> ProbeResult<()> {
        let quote = self.latest_quote()?;
        let mut definition = self.breakout(1, quote.ask)?;
        definition.nodes[1].step = WorkflowStep::Action {
            action: WorkflowAction::Enter {
                side: OrderSide::Buy,
                quantity: fixed((0, QUANTITY.1))?,
                protection: None,
            },
            next: None,
        };
        let refusal = self
            .trading
            .register_workflow(definition, WorkflowLayout::default());
        let stored = self.workflow(BREAKOUT_ID)?;
        self.evidence.check(
            "invalid_workflow_refused",
            matches!(refusal, Err(WorkflowError::Invalid(_))) && stored.is_none(),
            json!({ "refusal": refusal.err().map(|error| error.to_string()) }),
        )
    }

    /// Saves the breakout paused, arms it and waits for the live bid to reach its level.
    fn breakout_fires_on_a_live_cross(&mut self) -> ProbeResult<Entry> {
        let mut attempts = Vec::new();
        for revision in 1..=CROSS_ATTEMPTS {
            let quote = self.latest_quote()?;
            let saved = owner(self.trading.register_workflow(
                self.breakout(revision, quote.ask)?,
                WorkflowLayout::default(),
            ))?;
            let armed = owner(self.trading.arm_workflow(BREAKOUT_ID.to_string(), revision))?;
            if revision == 1 {
                self.layout_change_keeps_the_armed_revision(&armed)?;
                self.evidence.check(
                    "saved_paused_then_armed",
                    saved.state == WorkflowState::Paused
                        && armed.state == WorkflowState::Armed
                        && armed.blockers.is_empty(),
                    json!({
                        "saved_state": format!("{:?}", saved.state),
                        "armed_state": format!("{:?}", armed.state),
                        "blockers": format!("{:?}", armed.blockers),
                    }),
                )?;
            }
            let fired = poll_until(CROSS_WAIT, || {
                Ok(self
                    .workflow(BREAKOUT_ID)?
                    .filter(|view| !view.recent_firings.is_empty()))
            })?;
            attempts.push(json!({
                "revision": revision,
                "level_units": quote.ask.units(),
                "bid_units_when_armed": quote.bid.units(),
                "fired": fired.is_some(),
            }));
            if let Some(view) = fired {
                return self.check_breakout(&view, &attempts);
            }
            owner(
                self.trading
                    .pause_workflow(BREAKOUT_ID.to_string(), revision),
            )?;
        }
        self.evidence.check(
            "breakout_fired_on_live_cross",
            false,
            json!({ "attempts": attempts }),
        )?;
        Err(ProbeError::Invariant("breakout never fired".to_string()))
    }

    /// Moving blocks on the board changes only the layout: the armed revision stays armed.
    fn layout_change_keeps_the_armed_revision(&mut self, armed: &WorkflowView) -> ProbeResult<()> {
        let layout = WorkflowLayout {
            positions: std::iter::once(WorkflowNodeId::TRIGGER)
                .chain(armed.definition.nodes.iter().map(|node| node.id))
                .zip(0..)
                .map(|(node, column)| WorkflowNodePosition {
                    node,
                    x: column * 288,
                    y: 0,
                })
                .collect(),
        };
        let moved = owner(self.trading.set_workflow_layout(
            BREAKOUT_ID.to_string(),
            armed.definition.revision,
            layout.clone(),
        ))?;
        self.evidence.check(
            "layout_change_keeps_the_armed_revision",
            moved.layout == layout
                && moved.state == WorkflowState::Armed
                && moved.definition == armed.definition,
            json!({
                "state": format!("{:?}", moved.state),
                "revision": moved.definition.revision,
                "positions": moved.layout.positions.len(),
            }),
        )
    }

    fn check_breakout(
        &mut self,
        view: &WorkflowView,
        attempts: &[serde_json::Value],
    ) -> ProbeResult<Entry> {
        let firing = &view.recent_firings[0];
        let placed = match &firing.outcome {
            WorkflowFiringOutcome::Executed(steps) => match steps.as_slice() {
                [check, entry] => match (&check.outcome, &entry.outcome) {
                    (
                        WorkflowStepOutcome::Branch(true),
                        WorkflowStepOutcome::Action(WorkflowActionResult::Placed {
                            client_order_id,
                        }),
                    ) => Some(client_order_id.clone()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        self.evidence.check(
            "breakout_fired_on_live_cross",
            placed.is_some()
                && view.state == WorkflowState::Completed
                && view.total_firings == 1
                && firing.revision == view.definition.revision,
            json!({
                "attempts": attempts,
                "state": format!("{:?}", view.state),
                "outcome": format!("{:?}", firing.outcome),
                "fired_unix_nanos": firing.fired_unix_nanos,
            }),
        )?;
        let client_order_id =
            placed.ok_or_else(|| ProbeError::Invariant("breakout placed no order".to_string()))?;
        let fill = wait_for("breakout entry fill", QUOTE_WAIT, || {
            let Some(order) = find_order(&self.trading, &client_order_id)? else {
                return Ok(None);
            };
            if order.status != OrderStatus::Filled {
                return Ok(None);
            }
            Ok(self
                .current()?
                .fills
                .into_iter()
                .find(|fill| fill.order_id == order.id))
        })?;
        let matched = self
            .recorder
            .quote_at(BTC_ID, fill.provenance.observed_unix_nanos);
        let current = self.current()?;
        let bracket = current
            .managed_brackets
            .iter()
            .find(|bracket| bracket.entry_client_order_id == client_order_id);
        let position = self.position_units()?;
        self.evidence.check(
            "entry_filled_at_owner_quote_with_bracket",
            matched.is_some_and(|quote| quote.ask == fill.price)
                && fill.quantity == fixed(QUANTITY)?
                && position == QUANTITY.0
                && bracket.is_some_and(|bracket| {
                    bracket.status == BracketViewStatus::Active
                        && bracket.stop_client_order_id.is_some()
                        && bracket.target_client_order_ids.len() == 1
                }),
            json!({
                "client_order_id": client_order_id.as_str(),
                "fill_price_units": fill.price.units(),
                "recorded_ask_units": matched.map(|quote| quote.ask.units()),
                "position_units": position,
                "bracket_status": bracket.map(|bracket| format!("{:?}", bracket.status)),
            }),
        )?;
        Ok(Entry {
            client_order_id: client_order_id.as_str().to_string(),
        })
    }

    /// Arms a close at the next UTC minute and checks it flattens and cancels the bracket.
    fn time_of_day_closes_the_position(&mut self, entry: &Entry) -> ProbeResult<()> {
        let now = unix_nanos_now();
        let ahead = if now.rem_euclid(NANOS_PER_MINUTE) > 45 * NANOS_PER_SECOND {
            2
        } else {
            1
        };
        let due_minute = now.div_euclid(NANOS_PER_MINUTE) + ahead;
        let due = due_minute * NANOS_PER_MINUTE;
        let minute_of_day = u16::try_from(due_minute.rem_euclid(MINUTES_PER_DAY))
            .map_err(|error| ProbeError::Runtime(error.to_string()))?;
        owner(self.trading.register_workflow(
            self.definition(
                CLOSE_ID,
                1,
                "Probe timed close",
                WorkflowTrigger::TimeOfDay(WorkflowClockTime {
                    zone: WorkflowTimeZone::Utc,
                    minute_of_day,
                }),
                WorkflowCondition::Position(PositionRequirement::Long),
                &[WorkflowAction::Flatten, WorkflowAction::CancelOrders],
            )?,
            WorkflowLayout::default(),
        ))?;
        let armed = owner(self.trading.arm_workflow(CLOSE_ID.to_string(), 1))?;
        self.evidence.check(
            "time_workflow_due_at_next_minute",
            armed.state == WorkflowState::Armed && armed.next_due_unix_nanos == Some(due),
            json!({
                "minute_of_day": minute_of_day,
                "next_due_unix_nanos": armed.next_due_unix_nanos,
                "expected_due_unix_nanos": due,
            }),
        )?;
        let wait = Duration::from_nanos(
            u64::try_from(due - unix_nanos_now() + 30 * NANOS_PER_SECOND).unwrap_or_default(),
        );
        let fired = poll_until(wait, || {
            Ok(self
                .workflow(CLOSE_ID)?
                .filter(|view| !view.recent_firings.is_empty()))
        })?
        .ok_or_else(|| ProbeError::Invariant("time workflow never fired".to_string()))?;
        let firing = &fired.recent_firings[0];
        let closed = matches!(
            &firing.outcome,
            WorkflowFiringOutcome::Executed(steps)
                if matches!(
                    steps.iter().map(|step| &step.outcome).collect::<Vec<_>>().as_slice(),
                    [
                        WorkflowStepOutcome::Branch(true),
                        WorkflowStepOutcome::Action(WorkflowActionResult::Flattened),
                        WorkflowStepOutcome::Action(WorkflowActionResult::Cancelled { .. }),
                    ]
                )
        );
        let position = wait_for("flat after the timed close", QUOTE_WAIT, || {
            let units = self.position_units()?;
            Ok((units == 0).then_some(units))
        })?;
        let current = self.current()?;
        let open_orders = current
            .orders
            .iter()
            .filter(|order| order.account_id == self.account.id && order.status.is_open())
            .count();
        let bracket_active = current
            .managed_brackets
            .iter()
            .any(|bracket| bracket.entry_client_order_id.as_str() == entry.client_order_id);
        self.evidence.check(
            "time_workflow_flattened_and_cancelled",
            closed
                && fired.state == WorkflowState::Completed
                && (due..due + TIME_TRIGGER_TOLERANCE).contains(&firing.fired_unix_nanos)
                && position == 0
                && open_orders == 0
                && !bracket_active,
            json!({
                "outcome": format!("{:?}", firing.outcome),
                "fired_late_ms": (firing.fired_unix_nanos - due) / 1_000_000,
                "position_units": position,
                "open_orders": open_orders,
                "bracket_still_active": bracket_active,
            }),
        )
    }

    /// Restarts the trading owner on the same store with quotes flowing again and checks
    /// that both completed workflows recover without firing or placing anything.
    fn restart_fires_nothing_again(
        &mut self,
        market_service: &MarketService,
        guard: &mut TradingGuard,
        target: &FeedTarget,
        route: &MarketRoute,
        root: &DataRoot,
    ) -> ProbeResult<()> {
        let before = self.current()?;
        let orders_before = order_history(&self.trading)?.len();
        target.detach();
        let old = guard
            .0
            .take()
            .ok_or_else(|| ProbeError::Invariant("trading service is not running".to_string()))?;
        market(old.shutdown(Duration::from_secs(5)))?;
        let trading = start_trading(root.path())?;
        guard.0 = Some(trading.clone());
        aeris_probes::feed::wire_trading_feed(market_service, &trading, target)?;
        let epoch = self.recorder.advance_epoch();
        market(trading.set_ticket_interest(vec![route.clone()]))?;
        self.trading = trading;
        wait_for("BTC quote after restart", QUOTE_WAIT, || {
            Ok(self.recorder.first_quote_in_epoch(epoch, BTC_ID))
        })?;
        // Let live quotes run through the recovered owner before comparing.
        thread::sleep(Duration::from_secs(10));
        let after = self.current()?;
        let orders_after = order_history(&self.trading)?.len();
        let summary = |set: &WorkingSet| {
            let mut views = set
                .workflows
                .iter()
                .map(|view| {
                    (
                        view.definition.workflow_id.clone(),
                        view.state,
                        view.total_firings,
                        view.recent_firings.clone(),
                    )
                })
                .collect::<Vec<_>>();
            views.sort_by(|a, b| a.0.cmp(&b.0));
            views
        };
        let recovered = summary(&before) == summary(&after)
            && after.workflows.len() == 2
            && after
                .workflows
                .iter()
                .all(|view| view.state == WorkflowState::Completed && view.total_firings == 1);
        self.evidence.check(
            "restart_recovers_without_refiring",
            recovered && orders_before == orders_after && before.fills.len() == after.fills.len(),
            json!({
                "workflows": after.workflows.iter().map(|view| json!({
                    "id": view.definition.workflow_id,
                    "state": format!("{:?}", view.state),
                    "total_firings": view.total_firings,
                    "recent_firings": view.recent_firings.len(),
                })).collect::<Vec<_>>(),
                "orders_before": orders_before,
                "orders_after": orders_after,
                "fills_before": before.fills.len(),
                "fills_after": after.fills.len(),
            }),
        )
    }

    fn deleted_workflows_are_gone(&mut self) -> ProbeResult<()> {
        owner(self.trading.delete_workflow(BREAKOUT_ID.to_string()))?;
        owner(self.trading.delete_workflow(CLOSE_ID.to_string()))?;
        let arm_deleted = self.trading.arm_workflow(CLOSE_ID.to_string(), 1);
        let remaining = self.current()?.workflows.len();
        self.evidence.check(
            "deleted_workflows_are_gone",
            remaining == 0 && matches!(arm_deleted, Err(WorkflowError::UnknownWorkflow)),
            json!({
                "remaining": remaining,
                "arm_deleted": arm_deleted.err().map(|error| error.to_string()),
            }),
        )
    }
}
