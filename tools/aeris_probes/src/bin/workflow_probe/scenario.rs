//! The workflow scenario: save and arm a price workflow, let the live market cross it, let
//! a time-of-day workflow close the position, restart the trading owner and check that
//! nothing fires twice, delete the workflows, then let a position workflow flatten a
//! position the moment it opens.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use aeris_instruments::InstrumentId;
use aeris_market_data::ChartInterval;
use aeris_market_runtime::{
    InstrumentRequest, MarketService, MarketStream, SelectionRequest, StreamRequirements,
};
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
    BracketViewStatus, CreatePracticeAccount, ExecutionPolicy, HistoryQuery,
    MAXIMUM_WORKFLOW_TRADE_MINUTES, PositionRequirement, PriceDirection, TradingRetention,
    TradingService, TradingServiceConfig, WorkflowAction, WorkflowActionResult, WorkflowCheck,
    WorkflowClockTime, WorkflowCompare, WorkflowCondition, WorkflowDefinition, WorkflowError,
    WorkflowFiring, WorkflowFiringOutcome, WorkflowJoin, WorkflowLayout, WorkflowLimits,
    WorkflowNode, WorkflowNodeId, WorkflowNodePosition, WorkflowProtection, WorkflowRepeat,
    WorkflowScope, WorkflowState, WorkflowStep, WorkflowStepOutcome, WorkflowTimeZone,
    WorkflowTrigger, WorkflowView, WorkflowWeekdays, WorkingSet, clock::SystemClock,
};
use serde_json::json;

const PROVIDER: &str = "hyperliquid";
const BTC_ID: &str = "hyperliquid:perp:BTC";
const ETH_ID: &str = "hyperliquid:perp:ETH";
const ACCOUNT_NAME: &str = "Workflow Probe";
const STARTING_EQUITY: (i64, u8) = (10_000_000, 2); // $100,000.00
const QUANTITY: (i64, u8) = (1_000_000, 8); // 0.01 BTC
const ETH_QUANTITY: (i64, u8) = (100_000_000, 8); // 1 ETH
const PROTECTION_TICKS: u32 = 500;
const BREAKOUT_ID: &str = "probe-breakout";
const CLOSE_ID: &str = "probe-timed-close";
const TIMED_ENTRY_ID: &str = "probe-timed-entry";
const WATCH_ID: &str = "probe-position-watch";
const OPENER_ID: &str = "probe-opener";
/// Each attempt levels the trigger at the current ask; a market that only falls during one
/// attempt is retried with a new revision at the new ask.
const CROSS_ATTEMPTS: u32 = 5;
const CROSS_WAIT: Duration = Duration::from_secs(60);
const QUOTE_WAIT: Duration = Duration::from_secs(30);
/// An every-minute trigger is due within one minute.
const MINUTE_WAIT: Duration = Duration::from_secs(60);
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const NANOS_PER_MINUTE: i64 = 60 * NANOS_PER_SECOND;
const MINUTES_PER_DAY: i64 = 1_440;
/// The owner timer wakes at the due minute; this bounds how late a time trigger may fire.
const TIME_TRIGGER_TOLERANCE: i64 = 5 * NANOS_PER_SECOND;

fn fixed(value: (i64, u8)) -> ProbeResult<FixedPoint> {
    FixedPoint::try_new(value.0, value.1).map_err(|error| ProbeError::Runtime(error.to_string()))
}

/// Whether `firing` took the `held` branch and then ran one action that `done` accepts.
fn branch_then_action(
    firing: &WorkflowFiring,
    held: bool,
    done: fn(&WorkflowActionResult) -> bool,
) -> bool {
    let WorkflowFiringOutcome::Executed(steps) = &firing.outcome else {
        return false;
    };
    match steps.as_slice() {
        [branch, action] => {
            branch.outcome == WorkflowStepOutcome::Branch(held)
                && matches!(&action.outcome, WorkflowStepOutcome::Action(result) if done(result))
        }
        _ => false,
    }
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
    hyperliquid_route(BTC_ID, "BTC")
}

fn hyperliquid_route(instrument: &str, symbol: &str) -> ProbeResult<MarketRoute> {
    Ok(MarketRoute {
        provider_id: PROVIDER.to_string(),
        instrument_id: InstrumentId::try_new(instrument)
            .map_err(|error| ProbeError::Runtime(error.to_string()))?,
        provider_symbol: symbol.to_string(),
        display_symbol: format!("{symbol}-USDC"),
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

const CHART_CLIENT: u64 = 91;
const CHART_CONSUMER: u64 = 92;

/// A BTC chart as the desktop opens one (bars, trades, quotes and depth), drained on its
/// own thread so the workflow-only market shares the provider session with chart demand.
struct ChartLoad {
    service: MarketService,
    stop: Arc<AtomicBool>,
    events: Arc<AtomicU64>,
    drain: Option<thread::JoinHandle<()>>,
}

impl ChartLoad {
    fn open(service: &MarketService) -> ProbeResult<Self> {
        market(service.attach(CHART_CLIENT))?;
        market(service.register_consumer(CHART_CLIENT, 1, CHART_CONSUMER))?;
        market(
            service.select_and_demand(
                CHART_CLIENT,
                SelectionRequest {
                    consumer_id: CHART_CONSUMER,
                    generation: 1,
                    instrument: InstrumentRequest::Default,
                    interval: ChartInterval::Minute1,
                    streams: StreamRequirements::BARS
                        .with(MarketStream::Trades)
                        .with(MarketStream::Quotes)
                        .with(MarketStream::Depth),
                    viewport: None,
                },
            ),
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(AtomicU64::new(0));
        let drain = {
            let service = service.clone();
            let stop = Arc::clone(&stop);
            let events = Arc::clone(&events);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if !service.wait_for_events(CHART_CLIENT, Duration::from_millis(30)) {
                        continue;
                    }
                    if let Ok(drained) =
                        service.drain_events(CHART_CLIENT, &[(CHART_CONSUMER, 256)])
                    {
                        events.fetch_add(drained.len() as u64, Ordering::Relaxed);
                    }
                }
            })
        };
        Ok(Self {
            service: service.clone(),
            stop,
            events,
            drain: Some(drain),
        })
    }

    fn events(&self) -> u64 {
        self.events.load(Ordering::Relaxed)
    }
}

impl Drop for ChartLoad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(drain) = self.drain.take()
            && drain.join().is_err()
        {
            eprintln!("chart drain thread panicked");
        }
        if let Err(error) = self.service.detach(CHART_CLIENT) {
            eprintln!("chart client detach failed: {error}");
        }
    }
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
    probe.position_watch_flattens_what_opens()?;
    probe.workflow_only_market_entry_fills(market_service, &route)?;
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
            scope: WorkflowScope::Instrument(self.instrument.clone()),
            triggers: vec![trigger],
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
            WorkflowCondition::single(WorkflowCheck::Position(PositionRequirement::Flat)),
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

    /// Fires when the position opens and when it closes. While a position is open its time
    /// in trade is within the maximum, so it flattens; once flat the check is false and it
    /// cancels the working orders. Reading the open time blocks the run if it is unknown.
    fn position_watch(&self) -> ProbeResult<WorkflowDefinition> {
        let mut watch = self.definition(
            WATCH_ID,
            1,
            "Probe position watch",
            WorkflowTrigger::PositionOpened,
            WorkflowCondition::single(WorkflowCheck::TimeInTrade {
                compare: WorkflowCompare::AtMost,
                minutes: MAXIMUM_WORKFLOW_TRADE_MINUTES,
            }),
            &[WorkflowAction::Flatten],
        )?;
        let cancel = WorkflowNodeId(3);
        if let WorkflowStep::If { otherwise, .. } = &mut watch.nodes[0].step {
            *otherwise = Some(cancel);
        }
        watch.nodes.push(WorkflowNode {
            id: cancel,
            step: WorkflowStep::Action {
                action: WorkflowAction::CancelOrders,
                next: None,
            },
        });
        watch.triggers.push(WorkflowTrigger::PositionClosed);
        watch.limits.repeat = WorkflowRepeat::PerSession { maximum: 2 };
        Ok(watch)
    }

    /// At the next minute, while flat, buys without protection.
    fn opener(&self) -> ProbeResult<WorkflowDefinition> {
        self.definition(
            OPENER_ID,
            1,
            "Probe opener",
            WorkflowTrigger::Every { minutes: 1 },
            WorkflowCondition::single(WorkflowCheck::Position(PositionRequirement::Flat)),
            &[WorkflowAction::Enter {
                side: OrderSide::Buy,
                quantity: fixed(QUANTITY)?,
                protection: None,
            }],
        )
    }

    /// Arms the position watch while flat, then lets the opener buy: the watch fires once
    /// on the open and flattens, and once on that close and cancels, each reading the
    /// owner's open time without a blocker. Both workflows are then deleted.
    fn position_watch_flattens_what_opens(&mut self) -> ProbeResult<()> {
        owner(
            self.trading
                .register_workflow(self.position_watch()?, WorkflowLayout::default()),
        )?;
        let armed = owner(self.trading.arm_workflow(WATCH_ID.to_string(), 1))?;
        self.evidence.check(
            "position_watch_armed_while_flat",
            armed.state == WorkflowState::Armed
                && armed.blockers.is_empty()
                && armed.total_firings == 0
                && self.position_units()? == 0,
            json!({
                "state": format!("{:?}", armed.state),
                "blockers": format!("{:?}", armed.blockers),
            }),
        )?;
        owner(
            self.trading
                .register_workflow(self.opener()?, WorkflowLayout::default()),
        )?;
        owner(self.trading.arm_workflow(OPENER_ID.to_string(), 1))?;
        let view = wait_for("position watch firings", MINUTE_WAIT + QUOTE_WAIT, || {
            Ok(self
                .workflow(WATCH_ID)?
                .filter(|view| view.total_firings >= 2))
        })?;
        let opener = self.workflow(OPENER_ID)?;
        let flattened = view.recent_firings.iter().filter(|firing| {
            branch_then_action(firing, true, |result| {
                matches!(result, WorkflowActionResult::Flattened)
            })
        });
        let cancelled = view.recent_firings.iter().filter(|firing| {
            branch_then_action(firing, false, |result| {
                matches!(result, WorkflowActionResult::Cancelled { .. })
            })
        });
        let position = wait_for("flat after the watch", QUOTE_WAIT, || {
            let units = self.position_units()?;
            Ok((units == 0).then_some(units))
        })?;
        self.evidence.check(
            "position_triggers_fired_on_open_and_close",
            view.total_firings == 2
                && flattened.count() == 1
                && cancelled.count() == 1
                && opener
                    .as_ref()
                    .is_some_and(|opener| opener.total_firings == 1)
                && position == 0,
            json!({
                "total_firings": view.total_firings,
                "opener_firings": opener.as_ref().map(|opener| opener.total_firings),
                "position_units": position,
                "firings": view.recent_firings.iter().map(|firing| json!({
                    "fired_unix_nanos": firing.fired_unix_nanos,
                    "outcome": format!("{:?}", firing.outcome),
                })).collect::<Vec<_>>(),
            }),
        )?;
        owner(self.trading.delete_workflow(OPENER_ID.to_string()))?;
        owner(self.trading.delete_workflow(WATCH_ID.to_string()))
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

    /// Closes a long at `minute_of_day` UTC and on every minute, with the position check
    /// joined to a weekday check that holds on every day, as the board builds it.
    fn timed_close(&self, minute_of_day: u16) -> ProbeResult<WorkflowDefinition> {
        let mut close = self.definition(
            CLOSE_ID,
            1,
            "Probe timed close",
            WorkflowTrigger::TimeOfDay(WorkflowClockTime {
                zone: WorkflowTimeZone::Utc,
                minute_of_day,
            }),
            WorkflowCondition::joined(
                WorkflowJoin::All,
                &[
                    WorkflowCheck::Position(PositionRequirement::Long),
                    WorkflowCheck::Weekday {
                        zone: WorkflowTimeZone::Utc,
                        days: WorkflowWeekdays::from_bits(0b111_1111).ok_or_else(|| {
                            ProbeError::Runtime("every day is a weekday set".into())
                        })?,
                    },
                ],
            )
            .ok_or_else(|| ProbeError::Runtime("two checks join".into()))?,
            &[WorkflowAction::Flatten, WorkflowAction::CancelOrders],
        )?;
        close.triggers.push(WorkflowTrigger::Every { minutes: 1 });
        Ok(close)
    }

    /// Arms a close at the next UTC minute, triggered both by that time of day and by an
    /// every-minute trigger due at the same moment, and checks it fires once, flattens and
    /// cancels the bracket.
    fn time_of_day_closes_the_position(&mut self, entry: &Entry) -> ProbeResult<()> {
        let mut now = unix_nanos_now();
        // Late in a minute, wait for the next one so the every-minute trigger is not due
        // before the time of day.
        let into_minute = now.rem_euclid(NANOS_PER_MINUTE);
        if into_minute > 45 * NANOS_PER_SECOND {
            std::thread::sleep(Duration::from_nanos(
                u64::try_from(NANOS_PER_MINUTE - into_minute + NANOS_PER_SECOND)
                    .unwrap_or_default(),
            ));
            now = unix_nanos_now();
        }
        let due_minute = now.div_euclid(NANOS_PER_MINUTE) + 1;
        let due = due_minute * NANOS_PER_MINUTE;
        let minute_of_day = u16::try_from(due_minute.rem_euclid(MINUTES_PER_DAY))
            .map_err(|error| ProbeError::Runtime(error.to_string()))?;
        owner(
            self.trading
                .register_workflow(self.timed_close(minute_of_day)?, WorkflowLayout::default()),
        )?;
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
                && fired.total_firings == 1
                && (due..due + TIME_TRIGGER_TOLERANCE).contains(&firing.fired_unix_nanos)
                && position == 0
                && open_orders == 0
                && !bracket_active,
            json!({
                "outcome": format!("{:?}", firing.outcome),
                "total_firings": fired.total_firings,
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

    /// A market no ticket demands, as one picked by the page's market search: once its
    /// terms are known only the armed workflow brings its quotes back, a provider
    /// reconnect while it is armed resubscribes them, and the bracketed entry its timer
    /// places fills on a later quote.
    fn workflow_only_market_entry_fills(
        &mut self,
        market_service: &MarketService,
        btc_route: &MarketRoute,
    ) -> ProbeResult<()> {
        let eth_route = hyperliquid_route(ETH_ID, "ETH")?;
        let chart = ChartLoad::open(market_service)?;
        market(
            self.trading
                .set_ticket_interest(vec![btc_route.clone(), eth_route.clone()]),
        )?;
        wait_for("verified ETH terms", Duration::from_secs(45), || {
            Ok(self
                .current()?
                .instruments
                .into_iter()
                .find(|instrument| instrument.instrument_id == eth_route.instrument_id))
        })?;
        wait_for("live ETH quote", QUOTE_WAIT, || {
            Ok(self.recorder.latest_quote(ETH_ID))
        })?;
        market(self.trading.set_ticket_interest(vec![btc_route.clone()]))?;
        let armed = self.arm_timed_entry(&eth_route)?;
        let due = armed
            .next_due_unix_nanos
            .ok_or_else(|| ProbeError::Invariant("timed entry has no due time".to_string()))?;
        let generation_before = wait_for("armed ETH quote", QUOTE_WAIT, || {
            Ok(self.recorder.latest_quote(ETH_ID))
        })?
        .provider_generation;
        market(market_service.probe_fail_hyperliquid_websocket(0))?;
        let resubscribed = poll_until(Duration::from_secs(45), || {
            Ok(self
                .recorder
                .latest_quote(ETH_ID)
                .filter(|quote| quote.provider_generation > generation_before))
        })?;
        let rearmed_quote = self.lapse_timed_entry_interest(due)?;
        let wait = Duration::from_nanos(
            u64::try_from(due - unix_nanos_now() + 30 * NANOS_PER_SECOND).unwrap_or_default(),
        );
        let fired = poll_until(wait, || {
            Ok(self
                .workflow(TIMED_ENTRY_ID)?
                .filter(|view| !view.recent_firings.is_empty()))
        })?
        .ok_or_else(|| ProbeError::Invariant("timed entry never fired".to_string()))?;
        let outcome = &fired.recent_firings[0].outcome;
        let placed = match outcome {
            WorkflowFiringOutcome::Executed(steps) => {
                steps.iter().find_map(|step| match &step.outcome {
                    WorkflowStepOutcome::Action(WorkflowActionResult::Placed {
                        client_order_id,
                    }) => Some(client_order_id.clone()),
                    _ => None,
                })
            }
            _ => None,
        };
        let order = match &placed {
            Some(client_order_id) => poll_until(QUOTE_WAIT, || {
                Ok(find_order(&self.trading, client_order_id)?
                    .filter(|order| !order.status.is_open()))
            })?,
            None => None,
        };
        let current = self.current()?;
        let latest = self.recorder.latest_quote(ETH_ID);
        self.evidence.check(
            "workflow_only_market_entry_filled",
            resubscribed.is_some()
                && rearmed_quote.is_some()
                && order
                    .as_ref()
                    .is_some_and(|order| order.status == OrderStatus::Filled),
            json!({
                "generation_before_reconnect": generation_before,
                "resubscribed_generation": resubscribed.map(|quote| quote.provider_generation),
                "reconnected_before_due": resubscribed
                    .is_some_and(|quote| quote.received_unix_nanos < due),
                "rearmed_quote_generation": rearmed_quote.map(|quote| quote.provider_generation),
                "blockers_when_armed": format!("{:?}", armed.blockers),
                "outcome": format!("{outcome:?}"),
                "order_status": order.as_ref().map(|order| format!("{:?}", order.status)),
                "fired_unix_nanos": fired.recent_firings[0].fired_unix_nanos,
                "latest_eth_quote_received_unix_nanos":
                    latest.map(|quote| quote.received_unix_nanos),
                "market_observation_error": current.market_observation_error,
                "feed": self.recorder.debug_state(&[ETH_ID]),
                "chart_events": chart.events(),
            }),
        )?;
        owner(self.trading.delete_workflow(TIMED_ENTRY_ID.to_string()))
    }

    /// Registers and arms the bracketed ETH entry early in a minute, so the reconnect and the
    /// interest lapse that follow complete before the trigger is due.
    fn arm_timed_entry(&mut self, eth_route: &MarketRoute) -> ProbeResult<WorkflowView> {
        let mut definition = self.definition(
            TIMED_ENTRY_ID,
            1,
            "Probe timed entry",
            WorkflowTrigger::Every { minutes: 1 },
            WorkflowCondition::single(WorkflowCheck::Position(PositionRequirement::Flat)),
            &[WorkflowAction::Enter {
                side: OrderSide::Buy,
                quantity: fixed(ETH_QUANTITY)?,
                protection: Some(WorkflowProtection {
                    stop_ticks: 20,
                    target_ticks: 40,
                }),
            }],
        )?;
        definition.scope = WorkflowScope::Instrument(eth_route.instrument_id.clone());
        let into_minute = unix_nanos_now().rem_euclid(NANOS_PER_MINUTE);
        if into_minute > 20 * NANOS_PER_SECOND {
            thread::sleep(Duration::from_nanos(
                u64::try_from(NANOS_PER_MINUTE - into_minute + NANOS_PER_SECOND)
                    .unwrap_or_default(),
            ));
        }
        owner(
            self.trading
                .register_workflow(definition, WorkflowLayout::default()),
        )?;
        owner(self.trading.arm_workflow(TIMED_ENTRY_ID.to_string(), 1))
    }

    /// Lets the owner apply most of a minute of ETH quotes, then pauses the timed entry so its
    /// only ETH interest lapses within that provider generation, arms it again shortly before
    /// it is due and waits for the first quote of the restored interest. The restored quotes
    /// must apply although far fewer arrive before the entry's five-second window ends.
    fn lapse_timed_entry_interest(&mut self, due: i64) -> ProbeResult<Option<RecordedQuote>> {
        let lapse_at = due - 8 * NANOS_PER_SECOND;
        thread::sleep(Duration::from_nanos(
            u64::try_from(lapse_at - unix_nanos_now()).unwrap_or_default(),
        ));
        owner(self.trading.pause_workflow(TIMED_ENTRY_ID.to_string(), 1))?;
        thread::sleep(Duration::from_secs(1));
        let rearmed_at = unix_nanos_now();
        owner(self.trading.arm_workflow(TIMED_ENTRY_ID.to_string(), 1))?;
        poll_until(QUOTE_WAIT, || {
            Ok(self
                .recorder
                .latest_quote(ETH_ID)
                .filter(|quote| quote.received_unix_nanos > rearmed_at))
        })
    }
}
