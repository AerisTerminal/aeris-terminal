//! The M5.12 paper-trading scenario (VAL-TRD-069..073): live Hyperliquid quotes drive the
//! simulated venue through a marketable fill, resting orders, a managed bracket, IOC and
//! stale-feed rejections, a DAY expiry, a mid-run trading restart with a durable-state
//! digest comparison, quote resumption, flatten, ledger reconciliation, and export.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use aeris_instruments::{InstrumentClass, InstrumentId};
use aeris_market_runtime::MarketService;
use aeris_platform_runtime::data_root::{DataRoot, InstanceLock};
use aeris_probes::{
    Evidence, ProbeError, ProbeResult,
    feed::{FeedTarget, QuoteRecorder, RecordedQuote},
    market, prices, unix_nanos_now, wait_for,
};
use aeris_trading::{
    AccountEnvironment, ClientOrderId, FeeModel, FeeSchedule, Fill, FixedPoint, LedgerEntry,
    LedgerEntryKind, Order, OrderEvent, OrderEventKind, OrderEventReason, OrderSide, OrderStatus,
    OrderType, TimeInForce, TradingAccount, TradingAccountId, TradingProvenance,
    market::{FeedState, MarketRoute},
};
use aeris_trading_runtime::{
    BracketStrategyTemplate, BracketTarget, BracketView, BracketViewStatus, CreatePracticeAccount,
    ExecutionPolicy, FlattenScope, PlaceInlineBracket, PlaceOrder, PlacementOutcome, PlannedLeg,
    TradingEventCause, TradingInstrument, TradingRetention, TradingService, TradingServiceConfig,
    TradingSnapshot, TradingSubscription, clock::SystemClock,
};
use serde_json::json;

use super::Options;

const PROVIDER: &str = "hyperliquid";
const BTC_ID: &str = "hyperliquid:perp:BTC";
const SOL_ID: &str = "hyperliquid:perp:SOL";
const ACCOUNT_NAME: &str = "M5.12 Paper Probe";
const QUOTE_WAIT: Duration = Duration::from_secs(30);
const FEE_PER_SIDE: (i64, u8) = (5, 2); // $0.05 per contract side
const BTC_QUANTITY: (i64, u8) = (20_000_000, 8); // 0.2 BTC
const SOL_QUANTITY: (i64, u8) = (100_000_000, 8); // 1 SOL
const STARTING_EQUITY: (i64, u8) = (10_000_000, 2); // $100,000.00

fn fixed(value: (i64, u8)) -> ProbeResult<FixedPoint> {
    FixedPoint::try_new(value.0, value.1).map_err(|error| ProbeError::Runtime(error.to_string()))
}

fn client_id(value: &str) -> ProbeResult<ClientOrderId> {
    ClientOrderId::try_new(value).map_err(|error| ProbeError::Runtime(error.to_string()))
}

fn instrument_id(value: &str) -> ProbeResult<InstrumentId> {
    InstrumentId::try_new(value).map_err(|error| ProbeError::Runtime(error.to_string()))
}

/// The probe declares durable route identities; verified terms arrive from the coordinator.
fn ticket_route(instrument: &str, symbol: &str) -> ProbeResult<MarketRoute> {
    Ok(MarketRoute {
        provider_id: PROVIDER.to_string(),
        instrument_id: instrument_id(instrument)?,
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

/// Snaps a fraction of a positive reference price to three significant figures so the
/// order price always sits on the provider's significant-figure grid.
fn snapped(
    reference: FixedPoint,
    numerator: u32,
    denominator: u32,
    round_up: bool,
) -> ProbeResult<FixedPoint> {
    let scaled = prices::scale_fraction(reference, numerator, denominator)
        .ok_or_else(|| ProbeError::Invariant("reference price could not be scaled".to_string()))?;
    prices::snap_significant(scaled, 3, round_up)
        .ok_or_else(|| ProbeError::Invariant("price could not be snapped to the grid".to_string()))
}

/// Mirrors the desktop: the owner stamps the current verified quote over this placeholder.
fn order_provenance(provider_generation: u64) -> TradingProvenance {
    TradingProvenance {
        venue_id: "aeris-sim".to_string(),
        provider_id: PROVIDER.to_string(),
        session_generation: provider_generation,
        source_sequence: 1,
        observed_unix_nanos: unix_nanos_now(),
    }
}

fn retryable_admission(error: &str) -> bool {
    error.contains("quote") || error.contains("session")
}

/// Places one order, retrying transient quote/session admission races inside a deadline.
fn place_order(trading: &TradingService, order: &PlaceOrder) -> ProbeResult<Order> {
    let deadline = Instant::now() + QUOTE_WAIT;
    loop {
        match trading.place_order(order.clone()) {
            Ok(placed) => return Ok(placed),
            Err(error) if retryable_admission(&error) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(200));
            }
            Err(error) => {
                return Err(ProbeError::Runtime(format!(
                    "order {} was rejected: {error}",
                    order.client_order_id.as_str()
                )));
            }
        }
    }
}

fn place_bracket(
    trading: &TradingService,
    bracket: &PlaceInlineBracket,
) -> ProbeResult<PlacementOutcome> {
    let deadline = Instant::now() + QUOTE_WAIT;
    loop {
        match trading.place_inline_bracket(bracket.clone()) {
            Ok(outcome) => return Ok(outcome),
            Err(error) if retryable_admission(&error) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(200));
            }
            Err(error) => {
                return Err(ProbeError::Runtime(format!(
                    "bracket entry {} was rejected: {error}",
                    bracket.entry.client_order_id.as_str()
                )));
            }
        }
    }
}

fn base_order(
    client: &str,
    account: &TradingAccountId,
    instrument: &InstrumentId,
    quantity: FixedPoint,
    generation: u64,
) -> ProbeResult<PlaceOrder> {
    Ok(PlaceOrder {
        client_order_id: client_id(client)?,
        account_id: account.clone(),
        instrument_id: instrument.clone(),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GoodTillCancelled,
        quantity,
        limit_price: None,
        stop_price: None,
        submitted_unix_nanos: unix_nanos_now(),
        provenance: order_provenance(generation),
    })
}

/// Best-effort bounded shutdown of the active trading owner on every exit path.
struct TradingGuard(Option<TradingService>);

impl TradingGuard {
    fn take(&mut self) -> Option<TradingService> {
        self.0.take()
    }
    fn replace(&mut self, service: TradingService) {
        self.0 = Some(service);
    }
}

impl Drop for TradingGuard {
    fn drop(&mut self) {
        if let Some(service) = self.0.take() {
            let _ = service.shutdown(Duration::from_secs(2));
        }
    }
}

/// Durable recovered state compared across the in-run restart. Volatile mark and timer
/// projections (`market_observation_error`, `account_pnl` unrealized, `equity_samples`,
/// `recovery_notices`, position `unrealized_pnl`) are excluded by design. The durable
/// revision is compared separately: identical verified Terms must not rewrite it.
struct StateDigest {
    instruments: Vec<TradingInstrument>,
    accounts: Vec<TradingAccount>,
    orders: Vec<Order>,
    order_events: Vec<OrderEvent>,
    fills: Vec<Fill>,
    positions: Vec<PositionDigest>,
    ledger_entries: Vec<LedgerEntry>,
    fee_schedules: Vec<(TradingAccountId, FeeSchedule)>,
    managed_brackets: Vec<BracketView>,
    planned_legs: Vec<PlannedLeg>,
}

#[derive(PartialEq, Eq)]
struct PositionDigest {
    account_id: TradingAccountId,
    instrument_id: InstrumentId,
    net_quantity: FixedPoint,
    average_entry_price: Option<FixedPoint>,
    realized_pnl: FixedPoint,
    last_fill_unix_nanos: i64,
}

impl StateDigest {
    /// Names the durable families whose recovered content differs after the restart.
    fn differences(&self, other: &Self) -> Vec<&'static str> {
        let mut differences = Vec::new();
        if self.instruments != other.instruments {
            differences.push("instruments");
        }
        if self.accounts != other.accounts {
            differences.push("accounts");
        }
        if self.orders != other.orders {
            differences.push("orders");
        }
        if self.order_events != other.order_events {
            differences.push("order_events");
        }
        if self.fills != other.fills {
            differences.push("fills");
        }
        if self.positions != other.positions {
            differences.push("positions");
        }
        if self.ledger_entries != other.ledger_entries {
            differences.push("ledger_entries");
        }
        if self.fee_schedules != other.fee_schedules {
            differences.push("fee_schedules");
        }
        if self.managed_brackets != other.managed_brackets {
            differences.push("managed_brackets");
        }
        if self.planned_legs != other.planned_legs {
            differences.push("planned_legs");
        }
        differences
    }
}
fn digest(snapshot: &TradingSnapshot) -> StateDigest {
    let mut instruments = snapshot.instruments.clone();
    instruments.sort_by(|a, b| a.instrument_id.as_str().cmp(b.instrument_id.as_str()));
    let mut accounts = snapshot.accounts.clone();
    accounts.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    let mut orders = snapshot.orders.clone();
    orders.sort_by(|a, b| a.client_order_id.as_str().cmp(b.client_order_id.as_str()));
    let mut order_events = snapshot.order_events.clone();
    order_events.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    let mut fills = snapshot.fills.clone();
    fills.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    let mut positions = snapshot
        .positions
        .iter()
        .map(|position| PositionDigest {
            account_id: position.account_id.clone(),
            instrument_id: position.instrument_id.clone(),
            net_quantity: position.net_quantity,
            average_entry_price: position.average_entry_price,
            realized_pnl: position.realized_pnl,
            last_fill_unix_nanos: position.last_fill_unix_nanos,
        })
        .collect::<Vec<_>>();
    positions
        .sort_by(|a, b| (&a.account_id, &a.instrument_id).cmp(&(&b.account_id, &b.instrument_id)));
    let mut ledger_entries = snapshot.ledger_entries.clone();
    ledger_entries.sort_by(|a, b| a.entry_id.cmp(&b.entry_id));
    let mut fee_schedules = snapshot.fee_schedules.clone();
    fee_schedules.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    let mut managed_brackets = snapshot.managed_brackets.clone();
    managed_brackets.sort_by(|a, b| a.bracket_id.cmp(&b.bracket_id));
    let mut planned_legs = snapshot.planned_legs.clone();
    planned_legs.sort_by(|a, b| a.client_order_id.as_str().cmp(b.client_order_id.as_str()));
    StateDigest {
        instruments,
        accounts,
        orders,
        order_events,
        fills,
        positions,
        ledger_entries,
        fee_schedules,
        managed_brackets,
        planned_legs,
    }
}

/// Creates or reuses the probe's practice account by display name.
fn practice_account(
    trading: &TradingService,
    evidence: &mut Evidence,
) -> ProbeResult<TradingAccount> {
    let snapshot = market(trading.snapshot())?;
    let existing = snapshot.accounts.iter().find(|account| {
        account.display_name == ACCOUNT_NAME && account.environment == AccountEnvironment::Simulated
    });
    let reused = existing.is_some();
    let account = match existing {
        Some(account) => account.clone(),
        None => market(trading.create_practice_account(CreatePracticeAccount {
            display_name: ACCOUNT_NAME.to_string(),
            starting_equity: fixed(STARTING_EQUITY)?,
        }))?,
    };
    evidence.check(
        "account_created",
        true,
        json!({
            "reused": reused,
            "account_id": account.id.as_str(),
            "environment": format!("{:?}", account.environment),
            "currency": account.currency,
        }),
    )?;
    Ok(account)
}

/// Installs the per-contract fee schedule once per account, unless the run is `--no-fees`.
fn apply_fee_schedule(
    trading: &TradingService,
    account: &TradingAccount,
    evidence: &mut Evidence,
    no_fees: bool,
) -> ProbeResult<()> {
    if !no_fees {
        let installed = market(trading.snapshot())?
            .fee_schedules
            .iter()
            .any(|(account_id, _)| *account_id == account.id);
        if !installed {
            let mut by_class = BTreeMap::new();
            by_class.insert(
                InstrumentClass::CryptoPerpetual,
                FeeModel::PerContract {
                    per_side: fixed(FEE_PER_SIDE)?,
                },
            );
            market(trading.set_fee_schedule(
                account.id.clone(),
                FeeSchedule {
                    by_class,
                    by_instrument: BTreeMap::new(),
                },
            ))?;
        }
    }
    evidence.check(
        "fee_schedule_applied",
        true,
        json!({"mode": if no_fees { "none" } else { "per_contract" }}),
    )
}

/// Runs the scenario inside one acquired validation root and market service.
///
/// # Errors
/// Returns the first failed check or runtime error; the envelope still records evidence.
pub fn run(root: &DataRoot, options: &Options, evidence: &mut Evidence) -> ProbeResult<()> {
    let _lock =
        InstanceLock::acquire(root).map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let market_service = market(MarketService::start())?;
    let result = run_with_market(&market_service, root, options, evidence);
    let started = Instant::now();
    let shutdown = market_service.shutdown(Duration::from_secs(10));
    let elapsed = started.elapsed().as_millis();
    let shutdown_check = evidence.check(
        "market_shutdown_latency_ms",
        shutdown.is_ok() && elapsed < 10_000,
        json!({"elapsed_ms": elapsed}),
    );
    result?;
    market(shutdown)?;
    shutdown_check
}

fn run_with_market(
    market_service: &MarketService,
    root: &DataRoot,
    options: &Options,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let recorder = Arc::new(QuoteRecorder::new());
    let target = Arc::new(FeedTarget::default());
    if options.no_quote {
        target.suppress_quotes_for(SOL_ID);
    }
    aeris_probes::feed::attach_trading_feed(
        market_service,
        Arc::clone(&recorder),
        Arc::clone(&target),
    )?;
    evidence.check("feed_attached", true, json!({}))?;

    let trading = start_trading(root.path())?;
    let mut guard = TradingGuard(Some(trading));
    scenario(
        &mut guard,
        market_service,
        root,
        options,
        evidence,
        &recorder,
        &target,
    )
}

fn scenario(
    guard: &mut TradingGuard,
    market_service: &MarketService,
    root: &DataRoot,
    options: &Options,
    evidence: &mut Evidence,
    recorder: &QuoteRecorder,
    target: &FeedTarget,
) -> ProbeResult<()> {
    let trading = guard
        .0
        .as_ref()
        .ok_or_else(|| ProbeError::Invariant("trading service is not running".to_string()))?
        .clone();
    aeris_probes::feed::wire_trading_feed(market_service, &trading, target)?;
    let btc_route = ticket_route(BTC_ID, "BTC")?;
    let sol_route = ticket_route(SOL_ID, "SOL")?;
    let btc = btc_route.instrument_id.clone();
    let sol = sol_route.instrument_id.clone();
    market(trading.set_ticket_interest(vec![btc_route.clone(), sol_route]))?;

    // The public session starts on demand: the trading quote interest is what starts
    // Hyperliquid, so the readiness wait happens only after interest is published.
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
    evidence.check(
        "market_connected",
        true,
        json!({"provider_generation": generation}),
    )?;

    // Verified terms for both instruments register from the live feed (or recover from
    // the store on a rerun against an existing validation root).
    let registered = wait_for("verified instrument terms", Duration::from_secs(45), || {
        let snapshot = market(trading.snapshot())?;
        let ready = [BTC_ID, SOL_ID].iter().all(|instrument| {
            snapshot
                .instruments
                .iter()
                .any(|registered| registered.instrument_id.as_str() == *instrument)
        });
        Ok(ready.then(|| {
            snapshot
                .instruments
                .iter()
                .map(|registered| registered.instrument_id.as_str().to_string())
                .collect::<Vec<_>>()
        }))
    })?;
    evidence.check("terms_registered", true, json!({"instruments": registered}))?;
    let account = practice_account(&trading, evidence)?;
    apply_fee_schedule(&trading, &account, evidence, options.no_fees)?;

    if options.no_quote {
        return no_quote_rejection(&trading, &account, &sol, generation, recorder, evidence);
    }

    let mut probe = Probe {
        trading,
        account,
        btc,
        sol,
        btc_route,
        recorder,
        evidence,
        no_fees: options.no_fees,
        first_post_restart_quote: None,
        root,
    };
    probe.wait_live_quotes()?;
    probe.market_buy()?;
    probe.resting_limit()?;
    probe.stop_limit()?;
    probe.inline_bracket()?;
    probe.ioc_not_marketable()?;
    probe.day_expiry()?;
    probe.stale_rejection(market_service)?;
    probe.restart_round_trip(market_service, guard, target)?;
    probe.flatten_all()?;
    probe.ledger_identity()?;
    probe.export()?;
    probe.finish(guard)
}

fn no_quote_rejection(
    trading: &TradingService,
    account: &TradingAccount,
    sol: &InstrumentId,
    generation: u64,
    recorder: &QuoteRecorder,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    wait_for("SOL live feed without an owner quote", QUOTE_WAIT, || {
        Ok((recorder.feed_state(SOL_ID) == Some(FeedState::Live)).then_some(()))
    })?;
    market(trading.status())?;
    let mut order = base_order(
        "m512-never-quoted-sol",
        &account.id,
        sol,
        fixed(SOL_QUANTITY)?,
        generation,
    )?;
    order.limit_price = Some(fixed((100_000_000, 8))?);
    let rejection = trading.place_order(order);
    let snapshot = market(trading.snapshot())?;
    evidence.check(
        "no_quote_rejected",
        rejection.as_ref().is_err_and(|error| {
            error.contains(&aeris_trading::RejectReason::QuoteUnavailable.to_string())
        }) && snapshot.orders.is_empty()
            && snapshot.fills.is_empty(),
        json!({
            "instrument": SOL_ID,
            "owner_quote_suppressed": true,
            "feed_live": recorder.feed_state(SOL_ID) == Some(FeedState::Live),
            "rejection": rejection.err(),
            "orders": snapshot.orders.len(),
            "fills": snapshot.fills.len(),
        }),
    )?;
    Err(ProbeError::Invariant(
        "no-quote negative mode verified QuoteUnavailable".to_string(),
    ))
}

/// The live probe context after preparation: one scenario step per method.
struct Probe<'a> {
    trading: TradingService,
    account: TradingAccount,
    btc: InstrumentId,
    sol: InstrumentId,
    btc_route: MarketRoute,
    recorder: &'a QuoteRecorder,
    evidence: &'a mut Evidence,
    no_fees: bool,
    first_post_restart_quote: Option<i64>,
    root: &'a DataRoot,
}

impl Probe<'_> {
    fn snapshot(&self) -> ProbeResult<TradingSnapshot> {
        market(self.trading.snapshot())
    }

    fn latest_quote(&self, instrument: &str) -> ProbeResult<RecordedQuote> {
        wait_for("fresh live quote", QUOTE_WAIT, || {
            Ok(self.recorder.latest_quote(instrument))
        })
    }

    fn wait_live_quotes(&self) -> ProbeResult<()> {
        let deadline = Instant::now() + QUOTE_WAIT;
        loop {
            if self.recorder.latest_quote(BTC_ID).is_some()
                && self.recorder.latest_quote(SOL_ID).is_some()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ProbeError::Invariant(format!(
                    "timed out waiting for fresh live quote: {}",
                    self.recorder.debug_state(&[BTC_ID, SOL_ID])
                )));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn btc_order(&self, client: &str, generation: u64) -> ProbeResult<PlaceOrder> {
        base_order(
            client,
            &self.account.id,
            &self.btc,
            fixed(BTC_QUANTITY)?,
            generation,
        )
    }

    /// (a) Marketable market buy: fills at the recorded ask of the consumed quote.
    fn market_buy(&mut self) -> ProbeResult<()> {
        let quote = self.latest_quote(BTC_ID)?;
        let order = place_order(
            &self.trading,
            &PlaceOrder {
                order_type: OrderType::Market,
                time_in_force: TimeInForce::Day,
                ..self.btc_order("m512-market-buy", quote.provider_generation)?
            },
        )?;
        let order_id = order.id.clone();
        let fill = wait_for("market buy fill", QUOTE_WAIT, || {
            Ok(market(self.trading.snapshot())?
                .fills
                .into_iter()
                .find(|fill| fill.order_id == order_id))
        })?;
        let matched = self
            .recorder
            .quote_at(BTC_ID, fill.provenance.observed_unix_nanos)
            .ok_or_else(|| {
                ProbeError::Invariant("fill provenance matches no recorded quote".to_string())
            })?;
        let age_ms = (fill.execution_unix_nanos - matched.received_unix_nanos) / 1_000_000;
        self.evidence.check(
            "market_buy_filled_at_ask",
            fill.price == matched.ask
                && fill.quantity == fixed(BTC_QUANTITY)?
                && fill.provenance.session_generation == matched.provider_generation
                && (-1_000..=10_000).contains(&age_ms),
            json!({
                "fill_price_units": fill.price.units(),
                "recorded_ask_units": matched.ask.units(),
                "quote_age_ms": age_ms,
                "session_generation": fill.provenance.session_generation,
                "source_sequence": fill.provenance.source_sequence,
            }),
        )
    }

    /// (b) Resting limit buy well below the market.
    fn resting_limit(&mut self) -> ProbeResult<()> {
        let reference = self.latest_quote(BTC_ID)?;
        let limit = snapped(reference.bid, 95, 100, false)?;
        let order = place_order(
            &self.trading,
            &PlaceOrder {
                limit_price: Some(limit),
                ..self.btc_order("m512-resting-limit", reference.provider_generation)?
            },
        )?;
        self.evidence.check(
            "resting_limit_working",
            order.status == OrderStatus::Working && order.filled_quantity.units() == 0,
            json!({
                "limit_units": limit.units(),
                "reference_bid_units": reference.bid.units(),
                "status": order.status.as_str(),
            }),
        )
    }

    /// (c) Stop-limit buy above the market: working, never triggered during the run.
    fn stop_limit(&mut self) -> ProbeResult<()> {
        let reference = self.latest_quote(BTC_ID)?;
        let stop = snapped(reference.ask, 102, 100, true)?;
        let order = place_order(
            &self.trading,
            &PlaceOrder {
                order_type: OrderType::StopLimit,
                limit_price: Some(stop),
                stop_price: Some(stop),
                ..self.btc_order("m512-stop-limit", reference.provider_generation)?
            },
        )?;
        self.evidence.check(
            "stop_limit_working_untriggered",
            order.status == OrderStatus::Working && order.triggered_unix_nanos.is_none(),
            json!({
                "stop_units": stop.units(),
                "reference_ask_units": reference.ask.units(),
                "status": order.status.as_str(),
            }),
        )
    }

    /// (d) Inline bracket with a resting entry: planned stop and target stay inactive.
    fn inline_bracket(&mut self) -> ProbeResult<()> {
        let reference = self.latest_quote(BTC_ID)?;
        let entry_price = snapped(reference.bid, 90, 100, false)?;
        let outcome = place_bracket(
            &self.trading,
            &PlaceInlineBracket {
                entry: PlaceOrder {
                    limit_price: Some(entry_price),
                    ..self.btc_order("m512-bracket-entry", reference.provider_generation)?
                },
                template: BracketStrategyTemplate {
                    template_id: "m512-inline-bracket".to_string(),
                    revision: 1,
                    name: "M5.12 probe bracket".to_string(),
                    stop_offset_ticks: 40,
                    targets: vec![BracketTarget {
                        offset_ticks: 80,
                        quantity_percent: 100,
                    }],
                    trailing_stop: None,
                    break_even: None,
                    enabled: true,
                },
            },
        )?;
        let PlacementOutcome::Managed(view) = outcome else {
            return Err(ProbeError::Invariant(
                "inline bracket was not locally managed".to_string(),
            ));
        };
        let snapshot = self.snapshot()?;
        let legs = snapshot
            .planned_legs
            .iter()
            .filter(|leg| leg.entry_client_order_id == view.entry_client_order_id)
            .collect::<Vec<_>>();
        let legs_inactive = legs.len() == 2
            && legs.iter().all(|leg| {
                snapshot.orders.iter().any(|order| {
                    order.client_order_id == leg.client_order_id
                        && order.status == OrderStatus::Inactive
                })
            });
        self.evidence.check(
            "bracket_legs_inactive",
            view.status == BracketViewStatus::AwaitingEntry && legs_inactive,
            json!({
                "bracket_id": view.bracket_id,
                "status": format!("{:?}", view.status),
                "planned_legs": legs.len(),
            }),
        )
    }

    /// (e) IOC limit buy priced off the ask: admission cancels it as not marketable.
    fn ioc_not_marketable(&mut self) -> ProbeResult<()> {
        let reference = self.latest_quote(BTC_ID)?;
        let limit = snapped(reference.ask, 99, 100, false)?;
        let order = place_order(
            &self.trading,
            &PlaceOrder {
                time_in_force: TimeInForce::ImmediateOrCancel,
                limit_price: Some(limit),
                ..self.btc_order("m512-ioc", reference.provider_generation)?
            },
        )?;
        let snapshot = self.snapshot()?;
        let events = snapshot
            .order_events
            .iter()
            .filter(|event| event.order_id == order.id)
            .collect::<Vec<_>>();
        let not_marketable = events.iter().any(|event| {
            event.kind == OrderEventKind::Cancelled
                && event.reason == Some(OrderEventReason::NotMarketable)
        });
        let no_fill = !snapshot.fills.iter().any(|fill| fill.order_id == order.id);
        self.evidence.check(
            "ioc_not_marketable_cancelled",
            order.status == OrderStatus::Cancelled
                && order.filled_quantity.units() == 0
                && not_marketable
                && no_fill,
            json!({
                "status": order.status.as_str(),
                "limit_units": limit.units(),
                "reference_ask_units": reference.ask.units(),
                "events": events.iter().map(|event| event.kind.as_str()).collect::<Vec<_>>(),
            }),
        )
    }

    /// (f) DAY limit buy: expiry is the next UTC midnight of the acceptance time.
    fn day_expiry(&mut self) -> ProbeResult<()> {
        let reference = self.latest_quote(BTC_ID)?;
        let limit = snapped(reference.bid, 97, 100, false)?;
        let order = place_order(
            &self.trading,
            &PlaceOrder {
                time_in_force: TimeInForce::Day,
                limit_price: Some(limit),
                ..self.btc_order("m512-day-limit", reference.provider_generation)?
            },
        )?;
        let expected_expiry = prices::next_utc_midnight(order.accepted_unix_nanos)
            .ok_or_else(|| ProbeError::Invariant("day deadline overflowed".to_string()))?;
        self.evidence.check(
            "day_expiry_utc_midnight_scheduled",
            order.status == OrderStatus::Working
                && order.expires_at_unix_nanos == Some(expected_expiry),
            json!({
                "accepted_unix_nanos": order.accepted_unix_nanos,
                "expires_at_unix_nanos": order.expires_at_unix_nanos,
                "expected_unix_nanos": expected_expiry,
            }),
        )
    }

    /// (g) A dropped public feed rejects a fresh SOL market order while its ticket
    /// interest stays installed; removing SOL interest then leaves BTC as the only
    /// live route for the remaining steps.
    fn stale_rejection(&mut self, market_service: &MarketService) -> ProbeResult<()> {
        // Four injected failures hold the socket down through several backoff steps so
        // the order lands while the feed is Stale but before the next session generation
        // clears the owner's session state.
        market(market_service.probe_fail_hyperliquid_websocket(4))?;
        let transition_deadline = Instant::now() + Duration::from_secs(20);
        let mut observed_not_live = false;
        while Instant::now() < transition_deadline {
            if self
                .recorder
                .feed_state(SOL_ID)
                .is_some_and(|state| state != FeedState::Live)
            {
                observed_not_live = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let stale_since = self.recorder.latest_quote(SOL_ID);
        let generation = stale_since.map_or(1, |quote| quote.provider_generation);
        let rejection = self.trading.place_order(PlaceOrder {
            order_type: OrderType::Market,
            time_in_force: TimeInForce::Day,
            ..base_order(
                "m512-stale-sol",
                &self.account.id,
                &self.sol,
                fixed(SOL_QUANTITY)?,
                generation,
            )?
        });
        let result = match rejection {
            Err(error) => {
                let recognized =
                    error.contains("stale") || error.contains("live market quote unavailable");
                self.evidence.check(
                    "stale_or_closed_rejections",
                    recognized,
                    json!({
                        "error": error,
                        "sol_feed_left_live_observed": observed_not_live,
                        "sol_last_quote_age_ms": stale_since
                            .map(|quote| (unix_nanos_now() - quote.received_unix_nanos) / 1_000_000),
                    }),
                )
            }
            Ok(order) => self.evidence.check(
                "stale_or_closed_rejections",
                false,
                json!({
                    "unexpected_status": order.status.as_str(),
                    "sol_feed_left_live_observed": observed_not_live,
                }),
            ),
        };
        result?;
        market(
            self.trading
                .set_ticket_interest(vec![self.btc_route.clone()]),
        )
    }

    /// Shuts the trading owner down, restarts it on the same store with the market
    /// service untouched, and compares the recovered durable state field by field.
    fn restart_round_trip(
        &mut self,
        market_service: &MarketService,
        guard: &mut TradingGuard,
        target: &FeedTarget,
    ) -> ProbeResult<()> {
        let pre_snapshot = self.snapshot()?;
        let pre_revision = pre_snapshot.revision;
        let pre = digest(&pre_snapshot);
        let pre_unrealized = self
            .snapshot()?
            .account_pnl
            .iter()
            .find(|pnl| pnl.account_id == self.account.id)
            .and_then(|pnl| pnl.unrealized);
        target.detach();
        let old = guard
            .take()
            .ok_or_else(|| ProbeError::Invariant("trading service is not running".to_string()))?;
        let started = Instant::now();
        let shutdown = old.shutdown(Duration::from_secs(5));
        let elapsed = started.elapsed().as_millis();
        self.evidence.check(
            "trading_restart_shutdown_ms",
            shutdown.is_ok() && elapsed < 5_000,
            json!({"elapsed_ms": elapsed}),
        )?;
        market(shutdown)?;

        let trading = start_trading(self.root.path())?;
        aeris_probes::feed::wire_trading_feed(market_service, &trading, target)
            .map_err(|error| ProbeError::Runtime(format!("post-restart feed wiring: {error}")))?;
        let restart_started_at = unix_nanos_now();
        let epoch = self.recorder.advance_epoch();
        trading
            .set_ticket_interest(vec![self.btc_route.clone()])
            .map_err(|error| ProbeError::Runtime(format!("post-restart interest: {error}")))?;
        let subscription = trading
            .subscribe()
            .map_err(|error| ProbeError::Runtime(format!("post-restart subscribe: {error}")))?;
        guard.replace(trading.clone());
        self.trading = trading;

        self.quotes_resume(epoch, restart_started_at, pre_unrealized, &subscription)?;
        let post_snapshot = self.snapshot()?;
        let post_revision = post_snapshot.revision;
        let post = digest(&post_snapshot);
        let differences = pre.differences(&post);
        self.evidence.check(
            "restart_state_identical",
            differences.is_empty() && post_revision == pre_revision,
            json!({
                "differences": differences,
                "pre_revision": pre_revision,
                "post_revision": post_revision,
                "orders": post.orders.len(),
                "order_events": post.order_events.len(),
                "fills": post.fills.len(),
                "positions": post.positions.len(),
                "ledger_entries": post.ledger_entries.len(),
                "planned_legs": post.planned_legs.len(),
            }),
        )
    }

    /// Quotes resume into the recovered owner without any UI state: the first quote of
    /// the new epoch arrives inside the freshness window and Mark publications follow.
    fn quotes_resume(
        &mut self,
        epoch: u64,
        restart_started_at: i64,
        pre_unrealized: Option<FixedPoint>,
        subscription: &TradingSubscription,
    ) -> ProbeResult<()> {
        let first_quote = wait_for("BTC quote after restart", QUOTE_WAIT, || {
            Ok(self.recorder.first_quote_in_epoch(epoch, BTC_ID))
        })?;
        self.first_post_restart_quote = Some(first_quote);
        let arrival_ms = (first_quote - restart_started_at) / 1_000_000;
        let mut marks = 0_u32;
        let mut other_events = 0_u32;
        let mut unrealized_changed = false;
        let deadline = Instant::now() + QUOTE_WAIT;
        while Instant::now() < deadline && !unrealized_changed && marks < 2 {
            if let Some(update) = subscription.recv_timeout(Duration::from_millis(500)) {
                if matches!(update.event.cause, TradingEventCause::Mark) {
                    marks += 1;
                    let current = update
                        .event
                        .working_set
                        .account_pnl
                        .iter()
                        .find(|pnl| pnl.account_id == self.account.id)
                        .and_then(|pnl| pnl.unrealized);
                    if current != pre_unrealized {
                        unrealized_changed = true;
                    }
                } else {
                    other_events += 1;
                }
            }
        }
        self.evidence.check(
            "quotes_resume_without_ui",
            (0..=30_000).contains(&arrival_ms) && marks > 0 && (unrealized_changed || marks >= 2),
            json!({
                "mark_events": marks,
                "other_events": other_events,
                "unrealized_changed": unrealized_changed,
                "pre_unrealized_units": pre_unrealized.map(FixedPoint::units),
                "first_quote_age_ms": (unix_nanos_now() - first_quote) / 1_000_000,
                "first_quote_received_unix_nanos": first_quote,
                "restart_started_unix_nanos": restart_started_at,
                "first_quote_arrival_ms": arrival_ms,
                "series_demand_installed": false,
                "recorder_debug": self.recorder.debug_state(&[BTC_ID, SOL_ID]),
            }),
        )
    }

    /// Flattens the open position and cancels every working order, ending flat.
    fn flatten_all(&mut self) -> ProbeResult<()> {
        let outcome = market(self.trading.flatten(FlattenScope::default()))?;
        let sell_fills = outcome
            .fills
            .iter()
            .filter(|fill| fill.side == OrderSide::Sell)
            .map(|fill| {
                let recorded = self.recorder.quote_at(
                    fill.instrument_id.as_str(),
                    fill.provenance.observed_unix_nanos,
                );
                let matches = recorded.is_some_and(|quote| {
                    fill.price == quote.bid
                        && fill.provenance.session_generation == quote.provider_generation
                        && self.first_post_restart_quote.is_some_and(|first| {
                            quote.received_unix_nanos >= first && fill.execution_unix_nanos >= first
                        })
                });
                json!({
                    "instrument": fill.instrument_id.as_str(),
                    "fill_price_units": fill.price.units(),
                    "recorded_bid_units": recorded.map(|quote| quote.bid.units()),
                    "quote_received_unix_nanos": recorded.map(|quote| quote.received_unix_nanos),
                    "first_post_restart_quote_unix_nanos": self.first_post_restart_quote,
                    "matches": matches,
                })
            })
            .collect::<Vec<_>>();
        self.evidence.check(
            "flatten_sells_at_recorded_bid",
            !sell_fills.is_empty() && sell_fills.iter().all(|fill| fill["matches"] == true),
            json!({ "sell_fills": sell_fills }),
        )?;
        wait_for("flat positions", Duration::from_secs(10), || {
            Ok(market(self.trading.snapshot())?
                .positions
                .iter()
                .all(|position| position.net_quantity.units() == 0)
                .then_some(()))
        })?;
        let cancelled = market(self.trading.cancel_all(None))?;
        wait_for("no working orders", Duration::from_secs(10), || {
            Ok(market(self.trading.snapshot())?
                .orders
                .iter()
                .all(|order| !order.status.is_open())
                .then_some(()))
        })?;
        let snapshot = self.snapshot()?;
        let flat = snapshot
            .positions
            .iter()
            .all(|position| position.net_quantity.units() == 0);
        let no_working = snapshot.orders.iter().all(|order| !order.status.is_open());
        self.evidence.check(
            "flatten_all_flat",
            outcome.incomplete.is_none() && outcome.unpriced.is_empty() && flat && no_working,
            json!({
                "flatten_fills": outcome.fills.len(),
                "cancelled_orders": cancelled.len(),
                "incomplete": outcome.incomplete,
            }),
        )
    }

    /// The fill fee the runtime's single-rounding per-contract formula produces.
    fn expected_fee(&self, fill: &Fill) -> ProbeResult<i128> {
        if self.no_fees {
            return Ok(0);
        }
        let fee = aeris_trading::fixed_point_from_i128_rounded(
            i128::from(FEE_PER_SIDE.0) * i128::from(fill.quantity.units()),
            FEE_PER_SIDE.1 + fill.quantity.scale(),
            self.account.currency_scale,
        )
        .map_err(|error| ProbeError::Runtime(error.to_string()))?;
        Ok(i128::from(fee.units()))
    }

    /// The account balance is exactly the sum of its ledger entries, and every fill's
    /// fee entries replicate the runtime's per-contract computation.
    fn ledger_identity(&mut self) -> ProbeResult<()> {
        let snapshot = self.snapshot()?;
        let entries = snapshot
            .ledger_entries
            .iter()
            .filter(|entry| entry.account_id == self.account.id)
            .collect::<Vec<_>>();
        let is_fee = |entry: &LedgerEntry| {
            matches!(
                entry.kind,
                LedgerEntryKind::Commission | LedgerEntryKind::ExchangeFee
            )
        };
        let scale_consistent = entries
            .iter()
            .all(|entry| entry.amount.scale() == self.account.currency_scale);
        let sum: i128 = entries
            .iter()
            .map(|entry| i128::from(entry.amount.units()))
            .sum();
        let deposit: i128 = entries
            .iter()
            .filter(|entry| entry.kind == LedgerEntryKind::Deposit)
            .map(|entry| i128::from(entry.amount.units()))
            .sum();
        let realized: i128 = entries
            .iter()
            .filter(|entry| entry.kind == LedgerEntryKind::RealizedPnl)
            .map(|entry| i128::from(entry.amount.units()))
            .sum();
        let fees: i128 = entries
            .iter()
            .filter(|entry| is_fee(entry))
            .map(|entry| i128::from(entry.amount.units()))
            .sum();
        let fees_negative = entries
            .iter()
            .filter(|entry| is_fee(entry))
            .all(|entry| entry.amount.units() <= 0);
        let balance = snapshot
            .account_pnl
            .iter()
            .find(|pnl| pnl.account_id == self.account.id)
            .and_then(|pnl| pnl.balance);
        let fills = snapshot
            .fills
            .iter()
            .filter(|fill| fill.account_id == self.account.id)
            .collect::<Vec<_>>();
        let mut fee_mismatch = None;
        for fill in &fills {
            let fee_recorded: i128 = entries
                .iter()
                .filter(|entry| entry.fill_id.as_ref() == Some(&fill.id) && is_fee(entry))
                .map(|entry| i128::from(entry.amount.units()))
                .sum();
            let expected = self.expected_fee(fill)?;
            if fee_recorded != -expected {
                fee_mismatch = Some(json!({
                    "fill_id": fill.id.as_str(),
                    "recorded_units": fee_recorded.to_string(),
                    "expected_units": (-expected).to_string(),
                }));
                break;
            }
        }
        self.evidence.check(
            "ledger_identity",
            scale_consistent
                && fee_mismatch.is_none()
                && fees_negative
                && balance.is_some_and(|balance| i128::from(balance.units()) == sum)
                && sum == deposit + realized + fees,
            json!({
                "balance_units": balance.map(FixedPoint::units),
                "ledger_sum_units": sum.to_string(),
                "deposit_units": deposit.to_string(),
                "realized_units": realized.to_string(),
                "fee_units": fees.to_string(),
                "fills_checked": fills.len(),
                "fee_mismatch": fee_mismatch,
            }),
        )
    }

    /// Durable replacement into the validation root, with exact fill and CSV parity.
    fn export(&mut self) -> ProbeResult<()> {
        let directory = self.root.path().join("export");
        std::fs::create_dir_all(&directory)?;
        aeris_platform_runtime::fs::write_file_durably(
            &directory.join("executions.json"),
            b"previous complete export\n",
        )?;
        market(self.trading.export(directory.clone()))?;
        let mut exported = Vec::new();
        let mut files_nonempty = true;
        for name in ["executions.json", "user_records.json", "executions.csv"] {
            let path = directory.join(name);
            let bytes = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
            if bytes == 0 {
                files_nonempty = false;
            }
            exported.push(json!({"file": name, "bytes": bytes}));
        }
        let executions = std::fs::read(directory.join("executions.json"))
            .ok()
            .and_then(|body| serde_json::from_slice::<Vec<serde_json::Value>>(&body).ok());
        let records = std::fs::read(directory.join("user_records.json"))
            .ok()
            .and_then(|body| serde_json::from_slice::<Vec<serde_json::Value>>(&body).ok());
        let csv = std::fs::read_to_string(directory.join("executions.csv")).ok();
        let snapshot = self.snapshot()?;
        let fill_parity = executions.as_ref().is_some_and(|rows| {
            rows.len() == snapshot.fills.len()
                && rows.iter().all(|row| {
                    snapshot.fills.iter().any(|fill| {
                        row["id"].as_str() == Some(fill.id.as_str())
                            && row["order_id"].as_str() == Some(fill.order_id.as_str())
                            && row["account_id"].as_str() == Some(fill.account_id.as_str())
                            && row["instrument_id"].as_str() == Some(fill.instrument_id.as_str())
                            && row["side"].as_str() == Some(fill.side.as_str())
                            && row["price"]["units"].as_i64() == Some(fill.price.units())
                            && row["price"]["scale"].as_u64() == Some(u64::from(fill.price.scale()))
                            && row["quantity"]["units"].as_i64() == Some(fill.quantity.units())
                            && row["quantity"]["scale"].as_u64()
                                == Some(u64::from(fill.quantity.scale()))
                            && row["execution_unix_nanos"].as_i64()
                                == Some(fill.execution_unix_nanos)
                            && row["provenance"]["observed_unix_nanos"].as_i64()
                                == Some(fill.provenance.observed_unix_nanos)
                    })
                })
        });
        let csv_parity = csv
            .as_ref()
            .zip(executions.as_ref())
            .is_some_and(|(csv, rows)| {
                csv.lines().skip(1).count() == rows.len()
                    && csv.lines().skip(1).zip(rows).all(|(line, row)| {
                        line == format!(
                            "{},{},{},{},{},{},{},{},{},{}",
                            row["id"].as_str().unwrap_or_default(),
                            row["order_id"].as_str().unwrap_or_default(),
                            row["account_id"].as_str().unwrap_or_default(),
                            row["instrument_id"].as_str().unwrap_or_default(),
                            row["side"].as_str().unwrap_or_default(),
                            row["price"]["units"],
                            row["price"]["scale"],
                            row["quantity"]["units"],
                            row["quantity"]["scale"],
                            row["execution_unix_nanos"]
                        )
                    })
            });
        let staged_files = std::fs::read_dir(&directory)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with('.')
                        && Path::new(name)
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
                })
            })
            .count();
        self.evidence.check(
            "export_atomic_parity",
            files_nonempty && fill_parity && csv_parity && records.is_some() && staged_files == 0,
            json!({
                "files": exported,
                "fill_rows": executions.as_ref().map(Vec::len),
                "snapshot_fills": snapshot.fills.len(),
                "user_record_rows": records.as_ref().map(Vec::len),
                "fill_parity": fill_parity,
                "csv_parity": csv_parity,
                "staged_files": staged_files,
                "previous_export_replaced": true,
            }),
        )
    }

    /// Records run metrics and shuts the trading owner down inside its bound.
    fn finish(&mut self, guard: &mut TradingGuard) -> ProbeResult<()> {
        let (total_quotes, push_failures) = self.recorder.statistics();
        self.evidence.metrics = json!({
            "quotes_recorded": total_quotes,
            "feed_push_failures": push_failures,
            "fee_mode": if self.no_fees { "none" } else { "per_contract" },
        });
        let started = Instant::now();
        let shutdown = self.trading.shutdown(Duration::from_secs(5));
        let _ = guard.take();
        let elapsed = started.elapsed().as_millis();
        self.evidence.check(
            "trading_shutdown_latency_ms",
            shutdown.is_ok() && elapsed < 5_000,
            json!({"elapsed_ms": elapsed}),
        )?;
        market(shutdown)
    }
}
