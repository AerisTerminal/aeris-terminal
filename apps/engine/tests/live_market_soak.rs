//! A long-running soak against the live Coinbase venue.
//!
//! This is the test the market-data fixes are actually accountable to. Unit
//! tests pin each rule in isolation; only a real feed, running for minutes
//! across timeframe and symbol switches, shows whether the rules hold together:
//! whether bar identity survives a history repair, whether a bucket roll extends
//! the series instead of replacing it, and whether anything ever puts the feed
//! into a state nothing recovers from.
//!
//! It needs the network and takes minutes, so it is `#[ignore]` by default:
//!
//! ```text
//! cargo test -p axiusflow_engine --test live_market_soak -- --ignored --nocapture
//! AXIUSFLOW_SOAK_SECONDS=600 cargo test -p axiusflow_engine --test live_market_soak -- --ignored --nocapture
//! ```

use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_coinbase_market_adapter::CoinbaseInterval;
use axiusflow_engine::MarketService;
use axiusflow_engine_protocol::{
    InstallProviderInstrument, MarketBar, SearchProviderInstruments, SelectProviderInstrument,
    SeriesCadence, SeriesKey, SeriesLoadState, WorkspaceState, envelope,
};

const CLIENT_ID: u64 = 1;
/// Where this gate records its own outcome, relative to the repository root.
///
/// The desktop conformance report reads it verbatim. A missing file means the
/// gate did not run, and "did not run" is never reported as a pass.
const GATE_REPORT_DIRECTORY: &str = ".cache/evidence";
const CONSUMER_ID: u64 = 1;
const COINBASE_ENTITLEMENT_ID: &str = "crypto_public_realtime";

/// Default soak length. The maintainer asked for five to ten minutes; six gives
/// several one-minute bucket rolls, which is where the append path is exercised.
const DEFAULT_SOAK_SECONDS: u64 = 360;

/// How often the chart switches timeframe.
const TIMEFRAME_SWITCH: Duration = Duration::from_secs(45);
/// How often the chart switches symbol.
const SYMBOL_SWITCH: Duration = Duration::from_secs(150);
/// How long a selection must have been live before silence counts against it.
const SETTLE: Duration = Duration::from_secs(15);
/// How far into a bucket the market must be before an absent open candle counts
/// as a defect rather than a bucket nothing has traded in yet.
const BUCKET_ROLL_GRACE: i64 = 5;
/// How long a fresh selection may take to produce its covering history.
const HISTORY_DEADLINE: Duration = Duration::from_secs(45);
/// How long a live series may go without publishing before that counts as dead.
///
/// BTC-USD and ETH-USD trade continuously, and a bucket nothing traded in is
/// carried forward rather than skipped, so a healthy feed publishes far more
/// often than this.
const LIVENESS_DEADLINE: Duration = Duration::from_mins(2);

/// How deep a timeframe's history must get before it counts as loaded.
///
/// A chart that paints four bars is the "this timeframe is broken" symptom.
/// The engine paints an interim series derived from compatible in-memory bars
/// so the chart is never blank, then repairs it from the provider; this is the
/// floor the repair has to clear.
const HISTORY_DEPTH: usize = 200;

/// Intervals the chart cycles through. All are realtime-capable.
///
/// 12h is early in the cycle because long buckets previously exposed a dead
/// history/live handoff after a timeframe switch.
const TIMEFRAMES: [u32; 5] = [60, 43_200, 300, 900, 3_600];
const SYMBOLS: [&str; 2] = ["BTC-USD", "ETH-USD"];

/// What the consumer has reconstructed for one demand generation.
///
/// This mirrors what the desktop's replay model does with the same stream, so
/// anything the model would reject shows up here as a failure.
struct SeriesFold {
    generation: u64,
    interval_seconds: i64,
    bars: BTreeMap<u64, MarketBar>,
    snapshots: u64,
    updates: u64,
    started: Instant,
    last_publication: Instant,
}

impl SeriesFold {
    fn new(generation: u64, interval_seconds: i64) -> Self {
        Self {
            generation,
            interval_seconds,
            bars: BTreeMap::new(),
            snapshots: 0,
            updates: 0,
            started: Instant::now(),
            last_publication: Instant::now(),
        }
    }

    fn last_sequence(&self) -> Option<u64> {
        self.bars.keys().next_back().copied()
    }

    /// Replaces the series with a covering snapshot, checking it is canonical.
    fn apply_snapshot(&mut self, bars: Vec<MarketBar>) -> Result<(), String> {
        check_canonical(&bars, self.interval_seconds)
            .map_err(|error| format!("covering snapshot is not canonical: {error}"))?;
        self.bars = bars
            .into_iter()
            .map(|bar| (bar.source_sequence, bar))
            .collect();
        self.snapshots += 1;
        self.last_publication = Instant::now();
        Ok(())
    }

    /// Appends one bar, or revises the forming one.
    ///
    /// A bar that is neither is a sequence gap: the transport dropped something
    /// the strict `+1` contract needed, which is the failure every one of these
    /// fixes exists to prevent.
    fn apply_update(&mut self, bar: MarketBar) -> Result<(), String> {
        if let Some(last) = self.last_sequence()
            && bar.source_sequence != last
            && bar.source_sequence != last + 1
        {
            return Err(format!(
                "sequence gap: series holds up to {last}, received {} ({} bars held)",
                bar.source_sequence,
                self.bars.len()
            ));
        }
        self.bars.insert(bar.source_sequence, bar);
        self.updates += 1;
        self.last_publication = Instant::now();
        Ok(())
    }

    fn assert_contiguous(&self) -> Result<(), String> {
        check_canonical(
            &self.bars.values().copied().collect::<Vec<_>>(),
            self.interval_seconds,
        )
    }
}

/// Every adjacent pair must advance by exactly one sequence and one bucket.
fn check_canonical(bars: &[MarketBar], interval_seconds: i64) -> Result<(), String> {
    for pair in bars.windows(2) {
        if pair[0].source_sequence + 1 != pair[1].source_sequence {
            return Err(format!(
                "sequence {} is followed by {}",
                pair[0].source_sequence, pair[1].source_sequence
            ));
        }
        let step = pair[1].exchange_timestamp_seconds - pair[0].exchange_timestamp_seconds;
        if step != interval_seconds {
            return Err(format!(
                "bucket at {} is followed by {} ({step}s apart, expected {interval_seconds}s)",
                pair[0].exchange_timestamp_seconds, pair[1].exchange_timestamp_seconds
            ));
        }
    }
    Ok(())
}

fn provider_series(
    provider: &str,
    instrument: &InstallProviderInstrument,
    interval_seconds: u32,
) -> SeriesKey {
    SeriesKey {
        provider: provider.to_string(),
        instrument_id: instrument.instrument_id.clone(),
        cadence_value: interval_seconds,
        definition_revision: 1,
        entitlement_id: instrument.entitlement_id.clone(),
        cadence: SeriesCadence::FixedSeconds as i32,
    }
}

/// Polls until `accept` matches, failing the test on any terminal payload.
fn poll_until(
    service: &MarketService,
    deadline: Duration,
    what: &str,
    mut accept: impl FnMut(&envelope::Payload) -> bool,
) -> envelope::Payload {
    let expiry = Instant::now() + deadline;
    loop {
        if let Some(event) = service
            .poll_event(CLIENT_ID, CONSUMER_ID)
            .expect("market poll succeeds")
        {
            if let Some(failure) = terminal_failure(&event) {
                panic!("{what} failed terminally: {failure}");
            }
            if accept(&event) {
                return event;
            }
        }
        assert!(Instant::now() < expiry, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Payloads the desktop turns into a dead chart.
fn terminal_failure(event: &envelope::Payload) -> Option<String> {
    match event {
        envelope::Payload::DemandError(error) => {
            Some(format!("demand error in {}: {}", error.stage, error.detail))
        }
        envelope::Payload::Fault(fault) => Some(format!("fault: {}", fault.redacted_detail)),
        envelope::Payload::SeriesState(state)
            if state.state == SeriesLoadState::Failed as i32
                || state.state == SeriesLoadState::Superseded as i32 =>
        {
            Some(format!(
                "series state {}: {}",
                state.state,
                state
                    .detail
                    .clone()
                    .unwrap_or_else(|| "no detail".to_string())
            ))
        }
        _ => None,
    }
}

/// Resolves one product through the engine's own catalog.
fn install_symbol(
    service: &MarketService,
    provider: &str,
    symbol: &str,
    generation: u64,
) -> InstallProviderInstrument {
    service
        .search_provider_instruments(
            CLIENT_ID,
            SearchProviderInstruments {
                consumer_id: CONSUMER_ID,
                search_generation: generation,
                provider: provider.to_string(),
                query: symbol.to_string(),
                maximum_results: 32,
            },
        )
        .expect("catalog search is accepted");
    let found = poll_until(
        service,
        Duration::from_secs(45),
        "catalog search",
        |event| {
            matches!(event, envelope::Payload::ProviderInstrumentSearchResult(result)
            if result.search_generation == generation)
        },
    );
    let envelope::Payload::ProviderInstrumentSearchResult(result) = found else {
        unreachable!("poll_until matched a search result");
    };
    let summary = result
        .instruments
        .iter()
        .find(|candidate| candidate.symbol == symbol)
        .unwrap_or_else(|| panic!("{symbol} is listed; got {:?}", result.instruments.len()));

    let entitlement_id = if provider == "rithmic" {
        format!("rithmic-test:{}:{}", summary.exchange, summary.symbol)
    } else {
        COINBASE_ENTITLEMENT_ID.to_string()
    };
    service
        .select_provider_instrument(
            CLIENT_ID,
            SelectProviderInstrument {
                consumer_id: CONSUMER_ID,
                selection_generation: generation,
                search_generation: generation,
                provider: provider.to_string(),
                symbol: summary.symbol.clone(),
                exchange: summary.exchange.clone(),
                entitlement_id,
            },
        )
        .expect("catalog selection is accepted");
    let selected = poll_until(
        service,
        Duration::from_secs(45),
        "catalog selection",
        |event| {
            matches!(event, envelope::Payload::ProviderInstrumentSelection(selection)
                if selection.instrument.is_some())
        },
    );
    let envelope::Payload::ProviderInstrumentSelection(selection) = selected else {
        unreachable!("poll_until matched a selection");
    };
    let instrument = selection
        .instrument
        .expect("selection carries an instrument");
    service
        .install_provider_instrument(&instrument)
        .expect("instrument installs");
    instrument
}

/// Starts one demand generation and waits for its covering history.
fn start_series(
    service: &MarketService,
    provider: &str,
    instrument: &InstallProviderInstrument,
    interval_seconds: u32,
    generation: u64,
) -> SeriesFold {
    let series = provider_series(provider, instrument, interval_seconds);
    let requested_at = Instant::now();
    service
        .set_demand(CLIENT_ID, CONSUMER_ID, generation, &series)
        .expect("series demand is accepted");

    let mut fold = SeriesFold::new(generation, i64::from(interval_seconds));
    let mut deepest = 0;
    let snapshot = poll_until(
        service,
        HISTORY_DEADLINE,
        &format!(
            "{HISTORY_DEPTH} bars of history for {} at {interval_seconds}s",
            instrument.provider_symbol
        ),
        |event| match event {
            envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == generation => {
                deepest = deepest.max(snapshot.bars.len());
                snapshot.bars.len() >= HISTORY_DEPTH
            }
            _ => false,
        },
    );
    let envelope::Payload::SeriesSnapshot(snapshot) = snapshot else {
        unreachable!("poll_until matched a snapshot");
    };
    fold.apply_snapshot(snapshot.bars)
        .unwrap_or_else(|error| panic!("initial history is unusable: {error}"));
    // Switch latency is recorded, never asserted against an absolute duration:
    // how long a venue takes to serve a page is the venue's business, and a
    // threshold here would fail the build for a slow morning rather than for a
    // defect. What matters is that the switch completes and the chart is not
    // blank while it does, which the caller checks.
    eprintln!(
        "[switch] {} at {interval_seconds}s loaded {} bars in {:?}; open candle: {}",
        instrument.provider_symbol,
        fold.bars.len(),
        requested_at.elapsed(),
        if carries_the_open_candle(&fold) {
            "present"
        } else {
            "not yet"
        }
    );
    fold
}

/// Fails if the series does not carry the candle the market is currently in.
///
/// This is the forming-candle handoff. A chart the trader has just selected must
/// open on the candle the market is in, not on one that begins at the first
/// trade after the click — the symptom is a candle whose open, high, low, and
/// volume are all wrong for as long as the bucket lasts.
///
/// The check is skipped in the first [`BUCKET_ROLL_GRACE`] of a bucket, where a
/// bucket that has genuinely not traded yet is indistinguishable from a broken
/// handoff, and until the selection has settled — a fresh selection is allowed
/// to show retained local history while its covering repair is still in flight,
/// which is the whole point of never leaving the chart blank.
fn assert_carries_the_open_candle(fold: &SeriesFold, symbol: &str) {
    if fold.started.elapsed() < SETTLE || in_the_bucket_roll_grace(fold) {
        return;
    }
    assert!(
        carries_the_open_candle(fold),
        "the open candle for {symbol} at {}s is missing: newest bucket is {:?}",
        fold.interval_seconds,
        fold.bars
            .values()
            .next_back()
            .map(|bar| bar.exchange_timestamp_seconds)
    );
}

/// Whether the series holds the bucket the market is currently in.
fn carries_the_open_candle(fold: &SeriesFold) -> bool {
    let Some(open_bucket) = open_bucket(fold.interval_seconds) else {
        return true;
    };
    fold.bars
        .values()
        .next_back()
        .is_some_and(|bar| bar.exchange_timestamp_seconds >= open_bucket)
}

/// Whether the current bucket is too young to distinguish a bucket nothing has
/// traded in yet from a broken handoff.
fn in_the_bucket_roll_grace(fold: &SeriesFold) -> bool {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .is_none_or(|now| now.rem_euclid(fold.interval_seconds) < BUCKET_ROLL_GRACE)
}

fn open_bucket(interval_seconds: i64) -> Option<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())?;
    Some(now - now.rem_euclid(interval_seconds))
}

/// Applies one pushed event to the fold, panicking on anything terminal.
fn absorb(fold: &mut SeriesFold, event: envelope::Payload) {
    if let Some(failure) = terminal_failure(&event) {
        panic!(
            "feed died after {} snapshots and {} updates: {failure}",
            fold.snapshots, fold.updates
        );
    }
    match event {
        envelope::Payload::SeriesSnapshot(snapshot) if snapshot.generation == fold.generation => {
            fold.apply_snapshot(snapshot.bars)
                .unwrap_or_else(|error| panic!("{error}"));
        }
        envelope::Payload::SeriesUpdate(update) if update.generation == fold.generation => {
            if let Some(bar) = update.bar {
                fold.apply_update(bar)
                    .unwrap_or_else(|error| panic!("{error}"));
            }
        }
        _ => {}
    }
}

/// Closes out one demand generation, folding its counts into the totals.
fn retire(fold: &SeriesFold, snapshots: &mut u64, updates: &mut u64) {
    // Every selection must have streamed on its own, not merely inherited the
    // previous one's traffic. This is what "switching still leaves a live
    // chart" means. The final generation is exempt when the soak window closed
    // on it before a trade could plausibly arrive.
    assert!(
        fold.updates > 0 || fold.started.elapsed() < SETTLE,
        "generation {} on {}s ended with {} bars but never streamed a live update in {:?}",
        fold.generation,
        fold.interval_seconds,
        fold.bars.len(),
        fold.started.elapsed()
    );
    *snapshots += fold.snapshots;
    *updates += fold.updates;
}

/// The two properties that must hold on every poll of a healthy feed.
fn assert_streaming(fold: &SeriesFold, symbol: &str) {
    // A stall here is the exact symptom the maintainer reported: the chart
    // stops updating and never resumes.
    assert!(
        fold.last_publication.elapsed() < LIVENESS_DEADLINE,
        "streaming stopped for {:?} on {symbol} at {}s after {} snapshots and {} updates",
        fold.last_publication.elapsed(),
        fold.interval_seconds,
        fold.snapshots,
        fold.updates
    );
    fold.assert_contiguous()
        .unwrap_or_else(|error| panic!("held series is not canonical: {error}"));
    assert_carries_the_open_candle(fold, symbol);
}

/// Records this gate's outcome where the conformance report can read it.
///
/// It is written before the assertions that can fail and rewritten after they
/// pass, so a run that dies mid-flight leaves `failed` behind rather than the
/// previous run's `passed`.
fn record_gate(provider: &str, outcome: &str, detail: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(GATE_REPORT_DIRECTORY)
        .join(format!("live_market_gate_{provider}.json"));
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let recorded_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let _ = fs::write(
        &path,
        format!(
            "{{\n  \"provider\": \"{provider}\",\n  \"outcome\": \"{outcome}\",\n  \"recorded_at_unix_seconds\": {recorded_at},\n  \"detail\": \"{detail}\"\n}}\n"
        ),
    );
}

/// One provider's gate. Both are the same experiment against different venues.
struct Gate {
    provider: &'static str,
    symbols: &'static [&'static str],
    timeframes: &'static [u32],
}

/// What one soak observed, for the closing report.
#[derive(Default)]
struct GateTotals {
    switches: u32,
    snapshots: u64,
    updates: u64,
}

/// Starts the resident engine this gate drives.
fn start_gate_service(gate: &Gate) -> MarketService {
    let service = MarketService::start(&WorkspaceState {
        provider: gate.provider.to_string(),
        market: gate.symbols[0].to_string(),
        interval_seconds: gate.timeframes[0],
        ..WorkspaceState::default()
    })
    .expect("resident market engine starts");
    service.attach(CLIENT_ID).expect("client attaches");
    service
        .register_consumer(CLIENT_ID, 1, CONSUMER_ID)
        .expect("consumer registers");
    service
}

/// Drives one venue for the soak window, switching timeframe and symbol on a
/// schedule and folding every publication back into a series.
fn run_gate(gate: &Gate) {
    record_gate(
        gate.provider,
        "failed",
        "soak started and has not completed",
    );
    let soak = Duration::from_secs(
        std::env::var("AXIUSFLOW_SOAK_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_SOAK_SECONDS),
    );

    let service = start_gate_service(gate);

    let mut generation = 1_u64;
    let mut symbol_index = 0_usize;
    let mut timeframe_index = 0_usize;
    let mut instrument = install_symbol(&service, gate.provider, gate.symbols[0], generation);
    let mut fold = start_series(
        &service,
        gate.provider,
        &instrument,
        gate.timeframes[0],
        generation,
    );

    let started = Instant::now();
    let mut next_timeframe_switch = started + TIMEFRAME_SWITCH;
    let mut next_symbol_switch = started + SYMBOL_SWITCH;
    let mut totals = GateTotals::default();

    while started.elapsed() < soak {
        while let Some(event) = service
            .poll_event(CLIENT_ID, CONSUMER_ID)
            .expect("market poll succeeds")
        {
            absorb(&mut fold, event);
        }
        assert_streaming(&fold, &instrument.provider_symbol);

        let now = Instant::now();
        let symbol_due = now >= next_symbol_switch;
        let timeframe_due = now >= next_timeframe_switch;
        if !symbol_due && !timeframe_due {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        generation += 1;
        retire(&fold, &mut totals.snapshots, &mut totals.updates);
        totals.switches += 1;
        if symbol_due {
            next_symbol_switch = now + SYMBOL_SWITCH;
            next_timeframe_switch = now + TIMEFRAME_SWITCH;
            symbol_index = (symbol_index + 1) % gate.symbols.len();
            eprintln!(
                "[{:>4}s] symbol -> {} (generation {generation})",
                started.elapsed().as_secs(),
                gate.symbols[symbol_index]
            );
            // A symbol change must reselect the live feed. If it does not, the
            // replacement series receives the previous symbol's trades or
            // nothing at all, and `start_series` times out here.
            instrument = install_symbol(
                &service,
                gate.provider,
                gate.symbols[symbol_index],
                generation,
            );
        } else {
            next_timeframe_switch = now + TIMEFRAME_SWITCH;
            timeframe_index = (timeframe_index + 1) % gate.timeframes.len();
            eprintln!(
                "[{:>4}s] timeframe -> {}s (generation {generation})",
                started.elapsed().as_secs(),
                gate.timeframes[timeframe_index]
            );
        }
        fold = start_series(
            &service,
            gate.provider,
            &instrument,
            gate.timeframes[timeframe_index],
            generation,
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    retire(&fold, &mut totals.snapshots, &mut totals.updates);
    service
        .shutdown(Duration::from_secs(10))
        .expect("engine shuts down");
    close_gate(gate, soak, started.elapsed(), &totals);
}

/// Reports what the soak did and records the gate's outcome.
fn close_gate(gate: &Gate, soak: Duration, elapsed: Duration, totals: &GateTotals) {
    let GateTotals {
        switches,
        snapshots,
        updates,
    } = *totals;
    eprintln!(
        "soaked {elapsed:?} on {}: {switches} switches, {snapshots} snapshots, {updates} updates",
        gate.provider
    );
    // One switch is lost to the final partial window, so the floor is derived
    // from the schedule rather than hardcoded, and short smoke runs stay honest.
    let expected_switches = u32::try_from(soak.as_secs() / TIMEFRAME_SWITCH.as_secs())
        .unwrap_or(u32::MAX)
        .saturating_sub(1);
    assert!(
        switches >= expected_switches,
        "the soak switched {switches} times, expected at least {expected_switches}"
    );
    record_gate(
        gate.provider,
        "passed",
        &format!("{switches} switches, {snapshots} snapshots, {updates} updates"),
    );
}

#[test]
#[ignore = "drives the live Coinbase venue for several minutes"]
fn live_coinbase_streams_across_timeframe_and_symbol_switches() {
    run_gate(&Gate {
        provider: "coinbase",
        symbols: &SYMBOLS,
        timeframes: &TIMEFRAMES,
    });
}

#[test]
#[ignore = "drives the live Coinbase monthly history/live handoff"]
fn live_coinbase_monthly_reaches_a_forming_update() {
    let gate = Gate {
        provider: "coinbase",
        symbols: &SYMBOLS,
        timeframes: &TIMEFRAMES,
    };
    let service = start_gate_service(&gate);
    let instrument = install_symbol(&service, "coinbase", "BTC-USD", 1);
    let generation = 1;
    service
        .set_demand(
            CLIENT_ID,
            CONSUMER_ID,
            generation,
            &SeriesKey {
                provider: "coinbase".to_string(),
                instrument_id: instrument.instrument_id,
                cadence_value: 1,
                definition_revision: 1,
                entitlement_id: instrument.entitlement_id,
                cadence: SeriesCadence::CalendarMonths as i32,
            },
        )
        .expect("monthly demand is accepted");

    let interval = CoinbaseInterval::Month1;
    let deadline = Instant::now() + HISTORY_DEADLINE;
    let mut snapshot = false;
    let mut live = false;
    let mut update = false;
    while Instant::now() < deadline && !(snapshot && live && update) {
        if let Some(event) = service
            .poll_event(CLIENT_ID, CONSUMER_ID)
            .expect("monthly market poll succeeds")
        {
            if let Some(failure) = terminal_failure(&event) {
                panic!("monthly stream failed terminally: {failure}");
            }
            match event {
                envelope::Payload::SeriesSnapshot(series) if series.generation == generation => {
                    for pair in series.bars.windows(2) {
                        assert_eq!(pair[0].source_sequence + 1, pair[1].source_sequence);
                        assert_eq!(
                            interval
                                .shift_bucket(pair[0].exchange_timestamp_seconds, 1)
                                .expect("monthly bucket advances"),
                            pair[1].exchange_timestamp_seconds
                        );
                    }
                    snapshot = !series.bars.is_empty();
                }
                envelope::Payload::SeriesState(state)
                    if state.generation == generation
                        && state.state == SeriesLoadState::Live as i32 =>
                {
                    live = true;
                }
                envelope::Payload::SeriesUpdate(tail) if tail.generation == generation => {
                    let bar = tail.bar.expect("monthly update carries a bar");
                    assert_eq!(
                        interval
                            .bucket_start(bar.exchange_timestamp_seconds)
                            .expect("monthly update is bucketed"),
                        bar.exchange_timestamp_seconds
                    );
                    update = true;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        snapshot,
        "monthly demand never produced a covering snapshot"
    );
    assert!(live, "monthly demand never reached the live state");
    assert!(update, "monthly demand never produced a forming update");
    service
        .shutdown(Duration::from_secs(10))
        .expect("engine shuts down");
}

/// The Rithmic counterpart of the Coinbase gate.
///
/// It needs credentials in the native vault and a session the venue will accept,
/// so it only runs on a credentialed runner:
///
/// ```text
/// cargo test -p axiusflow_engine --test live_market_soak -- --ignored --nocapture rithmic
/// ```
#[test]
#[ignore = "drives the live Rithmic venue and needs vault credentials"]
fn live_rithmic_streams_across_timeframe_and_instrument_switches() {
    run_gate(&Gate {
        provider: "rithmic",
        symbols: &RITHMIC_SYMBOLS,
        timeframes: &RITHMIC_TIMEFRAMES,
    });
}

/// Intervals the Rithmic chart cycles through.
const RITHMIC_TIMEFRAMES: [u32; 3] = [60, 300, 900];
/// Front-month contract roots the Rithmic gate switches between.
///
/// These are searched through the engine's own catalog, so a rolled contract
/// resolves to whatever the venue currently lists for the root.
const RITHMIC_SYMBOLS: [&str; 2] = ["MNQ", "MES"];
