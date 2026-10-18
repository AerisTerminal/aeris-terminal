//! M5.12 cTrader demo probe: places and cancels one far-from-market limit order on the
//! reconciled cTrader demo account through the real bounded venue relay, over the stored
//! demo connection borrowed with `--validation-allow-stored-ctrader`. The probe never
//! starts browser authorization, refuses any live environment, and masks account
//! identifiers in evidence. Non-shipping validation tooling.

use std::{
    error::Error,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use aeris_contracts::{
    InstrumentSearchCategories, SearchProviderInstruments, SelectProviderInstrument,
};
use aeris_instruments::InstrumentId;
use aeris_market_runtime::{
    CtraderVenueLink, MarketRuntimeEvent, MarketService,
    market_service::instrument_terms_from_install,
};
use aeris_platform_runtime::data_root::{
    DataRoot, DataRootMode, InstanceLock, initialize_data_root,
};
use aeris_probes::{
    Envelope, Evidence, ProbeError, ProbeResult,
    feed::{FeedTarget, QuoteRecorder, RecordedQuote},
    market, unix_nanos_now, validate_paths, wait_for,
};
use aeris_trading::{
    AccountEnvironment, ClientOrderId, FixedPoint, OrderSide, OrderStatus, OrderType, TimeInForce,
    TradingAccount, TradingProvenance,
};
use aeris_trading_runtime::{
    ExecutionPolicy, PlaceOrder, TradingRetention, TradingService, TradingServiceConfig,
    TradingSnapshot, clock::SystemClock,
};
use serde_json::json;

const PROVIDER: &str = "ctrader";
const ENTITLEMENT: &str = "ctrader-authorized";
const SYMBOL_QUERY: &str = "EURUSD";
const DEMO_INSTRUMENT_PREFIX: &str = "ctrader:demo:";
const CLIENT: u64 = 1;
const WORKSPACE: u64 = 1;
const CONSUMER: u64 = 1;
const SESSION_WAIT: Duration = Duration::from_secs(60);
const CATALOG_WAIT: Duration = Duration::from_secs(60);
const QUOTE_WAIT: Duration = Duration::from_secs(30);
const WORKING_WAIT: Duration = Duration::from_secs(60);
const CANCEL_WAIT: Duration = Duration::from_secs(60);

pub struct Options {
    pub root: PathBuf,
    pub out: PathBuf,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, &'static str> {
        let mut root = None;
        let mut out = None;
        let mut borrow = false;
        let mut arguments = arguments.into_iter();
        while let Some(flag) = arguments.next() {
            match flag.as_str() {
                "--validation-root" if root.is_none() => {
                    let value = arguments.next().ok_or("missing probe option value")?;
                    root = Some(PathBuf::from(value));
                }
                "--out" if out.is_none() => {
                    let value = arguments.next().ok_or("missing probe option value")?;
                    out = Some(PathBuf::from(value));
                }
                "--validation-allow-stored-ctrader" if !borrow => borrow = true,
                _ => return Err("unknown, duplicate or unsafe probe option"),
            }
        }
        if !borrow {
            return Err(
                "the cTrader demo probe borrows the stored demo connection and requires \
                 --validation-allow-stored-ctrader",
            );
        }
        let root = root.ok_or("--validation-root is required")?;
        let out = out.ok_or("--out is required")?;
        validate_paths(&root, &out)?;
        Ok(Self { root, out })
    }
}

/// Masks any account identity to its trailing two characters before it enters evidence.
fn mask_account(value: &str) -> String {
    let length = value.chars().count();
    if length <= 2 {
        return "*".repeat(length.max(1));
    }
    let suffix = value
        .chars()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{}{suffix}", "*".repeat(length - 2))
}

/// Masks the account id segment of `ctrader:<environment>:<ctid>:<symbolId>`.
fn mask_instrument_id(instrument_id: &str) -> String {
    let segments: Vec<&str> = instrument_id.split(':').collect();
    if segments.len() != 4 {
        return "ctrader:redacted".to_string();
    }
    format!(
        "{}:{}:{}:{}",
        segments[0],
        segments[1],
        mask_account(segments[2]),
        segments[3]
    )
}

fn client_id(value: &str) -> ProbeResult<ClientOrderId> {
    ClientOrderId::try_new(value).map_err(|error| ProbeError::Runtime(error.to_string()))
}

fn start_trading(root: &std::path::Path) -> ProbeResult<TradingService> {
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

/// Re-expresses the contract tick size in the instrument's price scale, rounding away
/// from zero so the probe never quotes a price finer than the grid.
fn tick_units(instrument: &aeris_trading_runtime::TradingInstrument) -> Option<i64> {
    let tick = instrument.contract.tick_size?;
    let shift = i32::from(instrument.price_scale) - i32::from(tick.scale());
    let units = if shift >= 0 {
        i128::from(tick.units()) * 10_i128.checked_pow(u32::try_from(shift).ok()?)?
    } else {
        let divisor = 10_i128.checked_pow(u32::try_from(-shift).ok()?)?;
        i128::from(tick.units()).checked_add(divisor - 1)? / divisor
    };
    i64::try_from(units).ok().filter(|units| *units > 0)
}

/// Best-effort bounded shutdown of the trading owner on every exit path.
struct TradingGuard(Option<TradingService>);

impl Drop for TradingGuard {
    fn drop(&mut self) {
        if let Some(service) = self.0.take() {
            let _ = service.shutdown(Duration::from_secs(2));
        }
    }
}

/// Everything needed to submit the far limit and to record masked evidence.
struct OrderPlan {
    account: TradingAccount,
    masked_account: String,
    masked_instrument: String,
    instrument_id: InstrumentId,
    venue_id: String,
    limit: FixedPoint,
    tick: i64,
    quantity: FixedPoint,
    quote: RecordedQuote,
    venue_generation: u64,
    provider_generation: u64,
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
    let started = Instant::now();
    let shutdown = market_service.shutdown(Duration::from_secs(10));
    let elapsed = started.elapsed().as_millis();
    let check = evidence.check(
        "market_shutdown_latency_ms",
        shutdown.is_ok() && elapsed < 10_000,
        json!({"elapsed_ms": elapsed}),
    );
    result?;
    market(shutdown)?;
    check
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
    let trading = start_trading(root.path())?;
    let mut guard = TradingGuard(Some(trading));
    let trading = guard
        .0
        .as_ref()
        .ok_or_else(|| ProbeError::Invariant("trading service is not running".to_string()))?
        .clone();
    aeris_probes::feed::wire_trading_feed(market_service, &trading, &target)?;

    let (venue_generation, provider_generation) = open_venue(market_service, &trading, evidence)?;
    let instrument = discover_instrument(market_service)?;
    if !instrument.instrument_id.starts_with(DEMO_INSTRUMENT_PREFIX) {
        return Err(ProbeError::Invariant(format!(
            "selection resolved outside the demo host: {}",
            mask_instrument_id(&instrument.instrument_id)
        )));
    }
    let account = reconcile_account(&trading, evidence)?;
    let plan = prepare_order(
        &trading,
        &recorder,
        &instrument,
        account,
        evidence,
        venue_generation,
        provider_generation,
    )?;
    place_and_cancel(&trading, &plan, evidence)?;
    finish(&trading, &mut guard, &recorder, &plan, evidence)
}

/// Attaches the trading owner's demo venue generation to the shared session relay and
/// waits for the cTrader demo session to open.
fn open_venue(
    market_service: &MarketService,
    trading: &TradingService,
    evidence: &mut Evidence,
) -> ProbeResult<(u64, u64)> {
    // The venue boundary mirrors the desktop: the owner mints the generation and the
    // bounded request receiver; the market runtime relays it over the shared session.
    let (generation, requests) = market(trading.attach_demo_venue())?;
    let inbox = trading.demo_venue_inbox();
    market_service.attach_ctrader_venue(CtraderVenueLink {
        generation,
        requests,
        events: Box::new(move |event| inbox.push(event)),
    });
    evidence.check(
        "venue_attached",
        true,
        json!({"venue_generation": generation}),
    )?;

    let provider_generation = wait_for("ctrader demo session", SESSION_WAIT, || {
        Ok(market(market_service.status())?
            .providers
            .iter()
            .find(|provider| provider.provider == PROVIDER)
            .map(|provider| provider.generation)
            .filter(|generation| *generation > 0))
    })?;
    evidence.check(
        "session_opened",
        true,
        json!({"provider_generation": provider_generation}),
    )?;
    Ok((generation, provider_generation))
}

/// Runs the bounded catalog search/select handshake and returns the first exact demo
/// match for the probe symbol.
fn discover_instrument(
    market_service: &MarketService,
) -> ProbeResult<aeris_contracts::InstallProviderInstrument> {
    market(market_service.attach(CLIENT))?;
    market(market_service.register_consumer(CLIENT, WORKSPACE, CONSUMER))?;
    let discovery = poll_catalog(market_service);
    let _ = market_service.detach(CLIENT);
    discovery
}

fn poll_catalog(
    market_service: &MarketService,
) -> ProbeResult<aeris_contracts::InstallProviderInstrument> {
    market(market_service.search_provider_instruments(
        CLIENT,
        SearchProviderInstruments {
            consumer_id: CONSUMER,
            search_generation: 1,
            startup_resolve: false,
            provider: PROVIDER.to_string(),
            query: SYMBOL_QUERY.to_string(),
            maximum_results: 32,
            categories: InstrumentSearchCategories::ALL,
        },
    ))?;
    let deadline = Instant::now() + CATALOG_WAIT;
    while Instant::now() < deadline {
        while let Some(event) = market(market_service.poll_event(CLIENT, CONSUMER))? {
            match event.value {
                MarketRuntimeEvent::ProviderInstrumentSearchResult(result)
                | MarketRuntimeEvent::ProviderInstrumentSearchPreview(result) => {
                    if result.search_generation != 1 {
                        continue;
                    }
                    if let Some(candidate) = result
                        .instruments
                        .iter()
                        .find(|item| item.display_symbol == SYMBOL_QUERY)
                    {
                        market(market_service.select_provider_instrument(
                            CLIENT,
                            SelectProviderInstrument {
                                consumer_id: CONSUMER,
                                selection_generation: 1,
                                search_generation: 1,
                                startup_resolve: false,
                                provider: PROVIDER.to_string(),
                                symbol: candidate.symbol.clone(),
                                exchange: candidate.exchange.clone(),
                                entitlement_id: ENTITLEMENT.to_string(),
                            },
                        ))?;
                    }
                }
                MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
                    return Ok(selection.instrument);
                }
                MarketRuntimeEvent::ProviderCatalogRejected(rejection) => {
                    return Err(ProbeError::Runtime(format!(
                        "cTrader catalog rejected the request ({:?}); the stored demo \
                         connection may need a human reconnect in the desktop",
                        rejection.reason
                    )));
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(ProbeError::Invariant(
        "timed out waiting for the ctrader demo instrument selection".to_string(),
    ))
}

/// Waits for the reconciled demo account; a visible live account is a hard refusal.
fn reconcile_account(
    trading: &TradingService,
    evidence: &mut Evidence,
) -> ProbeResult<TradingAccount> {
    let account = wait_for("reconciled demo account", SESSION_WAIT, || {
        let snapshot = market(trading.snapshot())?;
        if snapshot
            .accounts
            .iter()
            .any(|account| account.environment == AccountEnvironment::Live)
        {
            return Err(ProbeError::Invariant(
                "live account visible; the demo probe refuses live environments".to_string(),
            ));
        }
        Ok(snapshot
            .accounts
            .into_iter()
            .find(|account| account.environment == AccountEnvironment::Demo))
    })?;
    evidence.check(
        "account_reconciled",
        true,
        json!({
            "account_id": mask_account(account.id.as_str()),
            "currency": account.currency,
        }),
    )?;
    Ok(account)
}

/// Registers interest, waits for verified terms and a live quote, and derives the far
/// limit (half the ask, snapped down to the provider tick) plus the minimum quantity.
fn prepare_order(
    trading: &TradingService,
    recorder: &QuoteRecorder,
    instrument: &aeris_contracts::InstallProviderInstrument,
    account: TradingAccount,
    evidence: &mut Evidence,
    venue_generation: u64,
    provider_generation: u64,
) -> ProbeResult<OrderPlan> {
    let masked_instrument = mask_instrument_id(&instrument.instrument_id);
    evidence.check(
        "instrument_selected",
        true,
        json!({
            "instrument_id": masked_instrument,
            "display_symbol": instrument.display_symbol,
            "venue_id": instrument.venue_id,
        }),
    )?;
    let terms = instrument_terms_from_install(instrument).map_err(ProbeError::Runtime)?;
    let instrument_id = terms.route.instrument_id.clone();
    // Verified Terms delivered over the trading feed perform the durable registration.
    market(trading.set_ticket_interest(vec![terms.route.clone()]))?;
    let quote = wait_for("live ctrader quote", QUOTE_WAIT, || {
        Ok(recorder.latest_quote(instrument_id.as_str()))
    })?;
    let registered = wait_for("registered instrument", QUOTE_WAIT, || {
        Ok(market(trading.snapshot())?
            .instruments
            .into_iter()
            .find(|registered| registered.instrument_id == instrument_id))
    })?;
    let tick = tick_units(&registered).ok_or_else(|| {
        ProbeError::Invariant("instrument carries no usable tick grid".to_string())
    })?;
    let half = i128::from(quote.ask.units()) / 2;
    let snapped = half / i128::from(tick) * i128::from(tick);
    let limit = FixedPoint::try_new(
        i64::try_from(snapped).map_err(|_| {
            ProbeError::Invariant("far limit overflowed the price grid".to_string())
        })?,
        registered.price_scale,
    )
    .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let step = terms
        .quantity_increment
        .map_or(1, aeris_instruments::InstrumentDecimal::units)
        .max(1);
    let quantity = FixedPoint::try_new(step, registered.quantity_scale)
        .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    Ok(OrderPlan {
        masked_account: mask_account(account.id.as_str()),
        account,
        masked_instrument,
        instrument_id,
        venue_id: terms.route.venue_id.clone(),
        limit,
        tick,
        quantity,
        quote,
        venue_generation,
        provider_generation,
    })
}

/// Places the far limit, waits for the venue working acknowledgement, cancels it, and
/// asserts no fill, no position change, and no ledger movement for the account.
fn place_and_cancel(
    trading: &TradingService,
    plan: &OrderPlan,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let before: TradingSnapshot = market(trading.snapshot())?;
    let order = market(trading.place_order(PlaceOrder {
        client_order_id: client_id("m512-ctrader-far-limit")?,
        account_id: plan.account.id.clone(),
        instrument_id: plan.instrument_id.clone(),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GoodTillCancelled,
        quantity: plan.quantity,
        limit_price: Some(plan.limit),
        stop_price: None,
        submitted_unix_nanos: unix_nanos_now(),
        provenance: TradingProvenance {
            venue_id: plan.venue_id.clone(),
            provider_id: PROVIDER.to_string(),
            session_generation: plan.quote.provider_generation,
            source_sequence: plan.quote.source_sequence.unwrap_or(1),
            observed_unix_nanos: unix_nanos_now(),
        },
    }))?;
    let order_id = order.id.clone();
    let working = wait_for("venue working acknowledgement", WORKING_WAIT, || {
        let snapshot = market(trading.snapshot())?;
        let current = snapshot
            .orders
            .iter()
            .find(|candidate| candidate.id == order_id);
        if current.is_some_and(|candidate| {
            matches!(
                candidate.status,
                OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired
            )
        }) {
            return Err(ProbeError::Invariant(format!(
                "venue refused the far limit: {}",
                current.map_or_else(
                    || "order missing".to_string(),
                    |order| order.status.as_str().to_string(),
                )
            )));
        }
        Ok(current
            .filter(|candidate| candidate.status == OrderStatus::Working)
            .cloned())
    })?;
    market(trading.cancel_order(working.client_order_id.clone()))?;
    let cancelled = wait_for("venue cancel acknowledgement", CANCEL_WAIT, || {
        Ok(market(trading.snapshot())?
            .orders
            .iter()
            .find(|candidate| candidate.id == order_id)
            .filter(|candidate| candidate.status == OrderStatus::Cancelled)
            .cloned())
    })?;
    let after: TradingSnapshot = market(trading.snapshot())?;
    let no_fill = !after.fills.iter().any(|fill| fill.order_id == order_id);
    let positions_unchanged = before.positions == after.positions;
    let ledger_unchanged = before
        .ledger_entries
        .iter()
        .filter(|entry| entry.account_id == plan.account.id)
        .count()
        == after
            .ledger_entries
            .iter()
            .filter(|entry| entry.account_id == plan.account.id)
            .count();
    evidence.check(
        "far_limit_placed_and_cancelled",
        cancelled.status == OrderStatus::Cancelled
            && cancelled.filled_quantity.units() == 0
            && no_fill
            && positions_unchanged
            && ledger_unchanged,
        json!({
            "account_id": plan.masked_account,
            "instrument_id": plan.masked_instrument,
            "limit_units": plan.limit.units(),
            "limit_scale": plan.limit.scale(),
            "reference_ask_units": plan.quote.ask.units(),
            "tick_units": plan.tick,
            "quantity_units": plan.quantity.units(),
            "final_status": cancelled.status.as_str(),
            "fills": after.fills.iter().filter(|fill| fill.order_id == order_id).count(),
            "positions_unchanged": positions_unchanged,
        }),
    )
}

/// Records run metrics and closes the trading session inside its bound.
fn finish(
    trading: &TradingService,
    guard: &mut TradingGuard,
    recorder: &QuoteRecorder,
    plan: &OrderPlan,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let (total_quotes, push_failures) = recorder.statistics();
    evidence.metrics = json!({
        "quotes_recorded": total_quotes,
        "feed_push_failures": push_failures,
        "venue_generation": plan.venue_generation,
        "provider_generation": plan.provider_generation,
    });
    let started = Instant::now();
    let shutdown = trading.shutdown(Duration::from_secs(5));
    let _ = guard.0.take();
    let elapsed = started.elapsed().as_millis();
    evidence.check(
        "session_closed_cleanly",
        shutdown.is_ok() && elapsed < 5_000,
        json!({"elapsed_ms": elapsed}),
    )?;
    market(shutdown)
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = Options::parse(std::env::args().skip(1)).map_err(ProbeError::Argument)?;
    // Reject unsafe roots before collecting evidence or attempting to write output.
    let root = initialize_data_root(DataRootMode::Validation {
        root: options.root.clone(),
        borrow_stored_ctrader: true,
    })
    .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    // The probe sources are not committed while the milestone is under development;
    // the worktree state stays on the envelope as evidence without gating the outcome.
    let envelope = Envelope::collect_with_policy(
        "ctrader_demo",
        options.root.clone(),
        options.out.clone(),
        true,
        false,
    )?;
    let started_at = aeris_probes::utc_now().to_rfc3339();
    let mut evidence = Evidence::new();
    let result = run(root, &mut evidence);
    let passed = envelope.finish(&started_at, &mut evidence, &result)?;
    if !passed {
        return Err(Box::new(ProbeError::Invariant(
            "cTrader demo probe failed; see JSON evidence".to_string(),
        )));
    }
    println!("cTrader demo probe passed: {}", options.out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_account_keeps_only_the_trailing_two_characters() {
        assert_eq!(mask_account("12345678"), "******78");
        assert_eq!(mask_account("ab"), "**");
        assert_eq!(mask_account("a"), "*");
        assert_eq!(mask_account("ctrader-demo-987654"), "*****************54");
    }

    #[test]
    fn mask_instrument_id_hides_only_the_account_segment() {
        assert_eq!(
            mask_instrument_id("ctrader:demo:12345678:41"),
            "ctrader:demo:******78:41"
        );
        assert_eq!(mask_instrument_id("ctrader:demo"), "ctrader:redacted");
    }
}
