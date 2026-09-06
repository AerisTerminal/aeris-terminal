//! A long-running soak against the live Rithmic venue.
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
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_engine::MarketService;
use axiusflow_engine_protocol::{
    InstallProviderInstrument, MarketBar, ProviderInstrumentSummary, SearchProviderInstruments,
    SelectProviderInstrument, SeriesCadence, SeriesKey, SeriesLoadState, WorkspaceState, envelope,
};
use serde::Serialize;
use sha2::Digest as _;

const CLIENT_ID: u64 = 1;
/// Where this gate records its own outcome, relative to the repository root.
///
/// The desktop conformance report reads it verbatim. A missing file means the
/// gate did not run, and "did not run" is never reported as a pass.
const GATE_REPORT_DIRECTORY: &str = ".cache/evidence";
const GATE_REPORT_SCHEMA_VERSION: u32 = 1;
const GATE_EVIDENCE_SCOPE: &str = "engine_live_market_gate";
const CONSUMER_ID: u64 = 1;
/// Second consumer holding the liquid anchor series for the whole soak.
///
/// When the venue goes quiet on a slow market, the anchor distinguishes "the
/// engine stalled" (anchor silent too: fail) from "the venue had nothing to
/// say" (anchor flowing: the shared worker, session, and publish path are
/// proven live). Only multiplexed providers can hold two instruments at once;
/// single-selection venues leave this unused.
const ANCHOR_CONSUMER_ID: u64 = 2;

/// Default soak length. The maintainer asked for five to ten minutes; six gives
/// several one-minute bucket rolls, which is where the append path is exercised.
const DEFAULT_SOAK_SECONDS: u64 = 360;

/// How often the chart switches timeframe.
const TIMEFRAME_SWITCH: Duration = Duration::from_secs(45);
/// How often the chart switches symbol.
const SYMBOL_SWITCH: Duration = Duration::from_secs(150);
/// How long a selection must have been live before silence counts against it.
const SETTLE: Duration = Duration::from_secs(15);
/// How long a fresh selection may take to produce its covering history.
const HISTORY_DEADLINE: Duration = Duration::from_secs(45);
/// How long a live series may go without publishing before that counts as dead.
///
/// A healthy feed publishes comfortably inside this deadline.
const LIVENESS_DEADLINE: Duration = Duration::from_mins(2);

/// How deep a timeframe's history must get before it counts as loaded.
///
/// A chart that paints four bars is the "this timeframe is broken" symptom.
/// The engine paints an interim series derived from compatible in-memory bars
/// so the chart is never blank, then repairs it from the provider; this is the
/// floor the repair has to clear.
const HISTORY_DEPTH: usize = 200;
/// What the consumer has reconstructed for one demand generation.
///
/// This mirrors what the desktop's replay model does with the same stream, so
/// anything the model would reject shows up here as a failure.
struct SeriesFold {
    generation: u64,
    interval_seconds: i64,
    instrument_id: String,
    initial_open_bucket: Option<i64>,
    bars: BTreeMap<u64, MarketBar>,
    snapshots: u64,
    updates: u64,
    /// Live trades published for this series on this generation. Candle-push
    /// cadence is venue-controlled and varies wildly across markets (core
    /// perps revise sub-second; spot pairs can go a minute between pushes),
    /// so a window shorter than the venue's cadence cannot demand a candle
    /// revise. Trades ride the same feed for the same symbol, so they prove
    /// the selection streams on its own just as well.
    trades: u64,
    started: Instant,
    last_publication: Instant,
}

impl SeriesFold {
    fn new(generation: u64, interval_seconds: i64, instrument_id: String) -> Self {
        Self {
            generation,
            interval_seconds,
            instrument_id,
            initial_open_bucket: None,
            bars: BTreeMap::new(),
            snapshots: 0,
            updates: 0,
            trades: 0,
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

/// Polls like [`poll_until`], but a catalog rejection re-issues the demand
/// instead of running out the deadline: the engine retires catalog demand
/// on generation advances, and the recovery is to re-demand, not to wait.
fn poll_catalog(
    service: &MarketService,
    deadline: Duration,
    what: &str,
    resend: impl Fn(),
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
            if matches!(event, envelope::Payload::ProviderCatalogRejected(_)) {
                resend();
            } else if accept(&event) {
                return event;
            }
        }
        assert!(Instant::now() < expiry, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Resolves one product through the engine's own catalog.
fn install_symbol(
    service: &MarketService,
    provider: &str,
    symbol: &str,
    generation: u64,
) -> InstallProviderInstrument {
    let send_search = || {
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
    };
    send_search();
    let found = poll_catalog(
        service,
        Duration::from_secs(45),
        "catalog search",
        send_search,
        |event| {
            matches!(event, envelope::Payload::ProviderInstrumentSearchResult(result)
            if result.search_generation == generation)
        },
    );
    let envelope::Payload::ProviderInstrumentSearchResult(result) = found else {
        unreachable!("poll_until matched a search result");
    };
    let summary = catalog_candidate(provider, symbol, &result.instruments).unwrap_or_else(|| {
        panic!(
            "{symbol} has a selectable listing; got {:?}",
            result.instruments.len()
        )
    });

    let entitlement_id = if provider == "hyperliquid" {
        "hyperliquid-public".to_string()
    } else if provider == "rithmic" {
        format!("rithmic-test:{}:{}", summary.exchange, summary.symbol)
    } else {
        panic!("unsupported live-gate provider");
    };
    let send_selection = || {
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
                    entitlement_id: entitlement_id.clone(),
                },
            )
            .expect("catalog selection is accepted");
    };
    send_selection();
    let selection_description = format!(
        "catalog selection for {} on {}",
        summary.symbol, summary.exchange
    );
    let selected = poll_catalog(
        service,
        Duration::from_secs(45),
        &selection_description,
        send_selection,
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
    eprintln!(
        "[select] provider={} instrument={} venue={} entitlement={}",
        provider, instrument.instrument_id, instrument.venue_id, instrument.entitlement_id
    );
    service
        .install_provider_instrument(&instrument)
        .expect("instrument installs");
    instrument
}

/// Selects the actual front-month future behind a Rithmic product root. The
/// catalog also returns the root itself and calendar spreads; neither is a
/// referenceable outright contract. Other providers use their exact listing.
fn catalog_candidate<'a>(
    provider: &str,
    query: &str,
    instruments: &'a [ProviderInstrumentSummary],
) -> Option<&'a ProviderInstrumentSummary> {
    if provider != "rithmic" {
        return instruments
            .iter()
            .find(|candidate| candidate.symbol == query);
    }
    instruments
        .iter()
        .filter(|candidate| {
            candidate.symbol.starts_with(query)
                && candidate.symbol != query
                && !candidate.symbol.contains('-')
                && candidate.expiration_date.is_some()
        })
        .min_by_key(|candidate| candidate.expiration_date.as_deref())
}

#[test]
fn rithmic_gate_selects_the_front_month_outright_instead_of_the_product_root() {
    let listing = |symbol: &str, expiration_date: Option<&str>| ProviderInstrumentSummary {
        symbol: symbol.to_string(),
        exchange: "CME-Delayed".to_string(),
        name: None,
        product_code: Some("MNQ".to_string()),
        instrument_type: Some("FUTURE".to_string()),
        expiration_date: expiration_date.map(str::to_string),
    };
    let instruments = [
        listing("MNQ", None),
        listing("MNQU6-MNQZ6", Some("20260918")),
        listing("MNQZ6", Some("20261218")),
        listing("MNQU6", Some("20260918")),
    ];

    assert_eq!(
        catalog_candidate("rithmic", "MNQ", &instruments).map(|result| result.symbol.as_str()),
        Some("MNQU6")
    );
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

    let mut fold = SeriesFold::new(
        generation,
        i64::from(interval_seconds),
        instrument.instrument_id.clone(),
    );
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
    fold.initial_open_bucket = open_bucket(fold.interval_seconds);
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

/// Whether the series holds the wall-clock bucket, for diagnostic output only.
///
/// Absence is not itself a failure: Rithmic emits no candle for a bucket that
/// has not traded, and this gate has no independent trade feed with which to
/// prove otherwise. Continuity and bounded publication liveness remain asserted;
/// value/coverage qualification needs a separate authoritative oracle.
fn carries_the_open_candle(fold: &SeriesFold) -> bool {
    let Some(open_bucket) = open_bucket(fold.interval_seconds) else {
        return true;
    };
    fold.bars
        .values()
        .next_back()
        .is_some_and(|bar| bar.exchange_timestamp_seconds >= open_bucket)
}

/// Parses a provider decimal without sharing the adapter's decoder.
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
        envelope::Payload::OrderFlowUpdate(update)
            if update.generation == fold.generation
                && update.trade.is_some()
                && update
                    .series
                    .as_ref()
                    .is_some_and(|series| series.instrument_id == fold.instrument_id) =>
        {
            fold.trades += 1;
            fold.last_publication = Instant::now();
        }
        _ => {}
    }
}

/// Closes out one demand generation, folding its counts into the totals.
fn retire(
    fold: &SeriesFold,
    symbol: &'static str,
    anchor_flowed: bool,
    snapshots: &mut u64,
    updates: &mut u64,
    trades: &mut u64,
    symbol_live: &mut BTreeMap<&'static str, u64>,
) {
    // Every selection must have streamed on its own, not merely inherited the
    // previous one's traffic. This is what "switching still leaves a live
    // chart" means. Either candle tails or order-flow trades prove it: both
    // ride the live feed for this symbol on this generation, and candle-push
    // cadence is venue-controlled (slow spot markets can go a full window
    // without a revise). When the selection itself saw nothing but the
    // anchor streamed in the same window, the shared worker, session, and
    // publish path are proven live and the venue simply had nothing to say
    // for this symbol; when the anchor is silent too the engine stalled, and
    // that fails. The final generation is exempt when the soak window closed
    // on it before a trade could plausibly arrive.
    let live = fold.updates + fold.trades;
    assert!(
        live > 0 || anchor_flowed || fold.started.elapsed() < SETTLE,
        "generation {} on {}s ended with {} bars and a silent anchor after {:?}",
        fold.generation,
        fold.interval_seconds,
        fold.bars.len(),
        fold.started.elapsed()
    );
    if live == 0 && anchor_flowed {
        eprintln!(
            "generation {} on {}s saw no venue activity for {symbol}; anchor live",
            fold.generation, fold.interval_seconds
        );
    }
    if fold.started.elapsed() >= SETTLE
        && fold
            .initial_open_bucket
            .zip(open_bucket(fold.interval_seconds))
            .is_some_and(|(initial, current)| current > initial)
    {
        assert!(
            fold.snapshots > 1,
            "generation {} crossed a {}s bucket without an authoritative correction snapshot",
            fold.generation,
            fold.interval_seconds
        );
    }
    *snapshots += fold.snapshots;
    *updates += fold.updates;
    *trades += fold.trades;
    *symbol_live.entry(symbol).or_insert(0) += live;
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
}

/// Records this gate's outcome where the conformance report can read it.
///
/// It is written as incomplete before the assertions that can fail and rewritten
/// as completed after they pass, so an interrupted run cannot reuse the previous
/// run's completed result.
#[derive(Serialize)]
struct LiveMarketGateReport<'a> {
    schema_version: u32,
    evidence_scope: &'static str,
    provider: &'a str,
    outcome: GateOutcome,
    completion_state: GateCompletion,
    recorded_at_unix_seconds: u64,
    source_revision: String,
    source_clean: bool,
    binary_path: PathBuf,
    binary_sha256: String,
    detail: &'a str,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum GateOutcome {
    Passed,
    Failed,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum GateCompletion {
    Incomplete,
    Completed,
}

fn record_gate(
    provider: &str,
    outcome: GateOutcome,
    completion_state: GateCompletion,
    detail: &str,
) {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = repository
        .join(GATE_REPORT_DIRECTORY)
        .join(format!("live_market_gate_{provider}.json"));
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    // Remove the prior result before doing any other work. If this process is
    // interrupted from here onward, the candidate has no completed report.
    let _ = fs::remove_file(&path);
    let recorded_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (source_revision, source_clean) = source_provenance(&repository);
    let binary_path = PathBuf::from(format!("live_market_gate_{provider}.bin"));
    let binary_artifact = parent.join(&binary_path);
    let binary_sha256 = std::env::current_exe()
        .ok()
        .and_then(|executable| fs::copy(executable, &binary_artifact).ok())
        .and_then(|_| file_sha256_hex(&binary_artifact).ok())
        .unwrap_or_default();
    let report = LiveMarketGateReport {
        schema_version: GATE_REPORT_SCHEMA_VERSION,
        evidence_scope: GATE_EVIDENCE_SCOPE,
        provider,
        outcome,
        completion_state,
        recorded_at_unix_seconds: recorded_at,
        source_revision,
        source_clean,
        binary_path,
        binary_sha256,
        detail,
    };
    if let Ok(mut encoded) = serde_json::to_vec_pretty(&report) {
        encoded.push(b'\n');
        // Truncating the old report before writing means an interrupted run
        // can leave only an incomplete or malformed record, never a stale pass.
        let _ = fs::write(&path, encoded);
    }
}

fn source_provenance(repository: &Path) -> (String, bool) {
    let revision = git_output(repository, &["rev-parse", "HEAD"]);
    let clean = git_output(
        repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .is_some_and(|status| status.is_empty());
    let revision = revision.filter(|revision| {
        revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    });
    let revision_is_valid = revision.is_some();
    (revision.unwrap_or_default(), clean && revision_is_valid)
}

fn git_output(repository: &Path, arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}

fn file_sha256_hex(path: &Path) -> Result<String, std::io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 16 * 1_024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(lower_hex(hasher.finalize()))
}

fn lower_hex(bytes: impl IntoIterator<Item = u8>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.into_iter();
    let mut encoded = String::with_capacity(bytes.size_hint().0.saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// One provider's gate. Both are the same experiment against different venues.
struct Gate {
    provider: &'static str,
    symbols: &'static [&'static str],
    timeframes: &'static [u32],
    /// Liquid symbol held by the anchor consumer for the whole soak, if the
    /// provider multiplexes. Must be `symbols[0]`: the anchor rides the first
    /// install, so it never disturbs the switch schedule.
    anchor: Option<&'static str>,
}

/// What one soak observed, for the closing report.
#[derive(Default)]
struct GateTotals {
    switches: u32,
    snapshots: u64,
    updates: u64,
    trades: u64,
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
    if gate.anchor.is_some() {
        service
            .register_consumer(CLIENT_ID, 1, ANCHOR_CONSUMER_ID)
            .expect("anchor consumer registers");
    }
    service
}

/// Switches the gate to the next symbol on its own generation.
///
/// A symbol change must reselect the live feed. If it does not, the
/// replacement series receives the previous symbol's trades or nothing at
/// all, and `start_series` times out here.
fn switch_symbol(
    service: &MarketService,
    gate: &Gate,
    generation: u64,
    symbol_index: usize,
    started: Instant,
    now: Instant,
) -> (usize, InstallProviderInstrument, Instant, Instant) {
    let symbol_index = (symbol_index + 1) % gate.symbols.len();
    eprintln!(
        "[{:>4}s] symbol -> {} (generation {generation})",
        started.elapsed().as_secs(),
        gate.symbols[symbol_index]
    );
    let instrument = install_symbol(
        service,
        gate.provider,
        gate.symbols[symbol_index],
        generation,
    );
    (
        symbol_index,
        instrument,
        now + SYMBOL_SWITCH,
        now + TIMEFRAME_SWITCH,
    )
}

/// Demands the liquid anchor series on its own consumer for the whole soak.
///
/// The anchor rides the first install and never switches, so it adds no
/// schedule of its own; multiplexed venues stream it beside every selection.
fn demand_anchor(
    service: &MarketService,
    gate: &Gate,
    instrument: &InstallProviderInstrument,
) -> Option<SeriesFold> {
    gate.anchor.map(|_| {
        let series = provider_series(gate.provider, instrument, gate.timeframes[0]);
        service
            .set_demand(CLIENT_ID, ANCHOR_CONSUMER_ID, 1, &series)
            .expect("anchor demand is accepted");
        SeriesFold::new(
            1,
            i64::from(gate.timeframes[0]),
            instrument.instrument_id.clone(),
        )
    })
}

/// Polls both gate consumers once, folding every publication and checking the
/// anchor stays canonical. The anchor shares the worker, session, and publish
/// path, so its contiguity proof covers the machinery the switched series
/// relies on in quiet windows.
fn drain_gate_events(
    service: &MarketService,
    fold: &mut SeriesFold,
    anchor: Option<&mut SeriesFold>,
    symbol: &str,
) {
    while let Some(event) = service
        .poll_event(CLIENT_ID, CONSUMER_ID)
        .expect("market poll succeeds")
    {
        absorb(fold, event);
    }
    if let Some(anchor_fold) = anchor {
        while let Some(event) = service
            .poll_event(CLIENT_ID, ANCHOR_CONSUMER_ID)
            .expect("anchor poll succeeds")
        {
            absorb(anchor_fold, event);
        }
        anchor_fold
            .assert_contiguous()
            .unwrap_or_else(|error| panic!("anchor series is not canonical: {error}"));
    }
    assert_streaming(fold, symbol);
}

/// Drives one venue for the soak window, switching timeframe and symbol on a
/// schedule and folding every publication back into a series.
fn run_gate(gate: &Gate) {
    record_gate(
        gate.provider,
        GateOutcome::Failed,
        GateCompletion::Incomplete,
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

    // The anchor holds the liquid series for the whole soak on its own
    // consumer, so a quiet window on a slow market can be told apart from a
    // stalled engine. It rides the first install and never switches.
    let mut anchor = demand_anchor(&service, gate, &instrument);
    // Live publications each selection streamed on its own, keyed by symbol:
    // a subscription that never delivers anywhere in the soak is a broken
    // feed, not a quiet venue, no matter what any single window saw.
    let mut symbol_live: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut anchor_baseline = live_count(anchor.as_ref());

    let started = Instant::now();
    let mut next_timeframe_switch = started + TIMEFRAME_SWITCH;
    let mut next_symbol_switch = started + SYMBOL_SWITCH;
    let mut totals = GateTotals::default();

    while started.elapsed() < soak {
        drain_gate_events(
            &service,
            &mut fold,
            anchor.as_mut(),
            &instrument.provider_symbol,
        );
        let now = Instant::now();
        let symbol_due = now >= next_symbol_switch;
        let timeframe_due = now >= next_timeframe_switch;
        if !symbol_due && !timeframe_due {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        generation += 1;
        let anchor_now = live_count(anchor.as_ref());
        let anchor_flowed = anchor_now > anchor_baseline;
        anchor_baseline = anchor_now;
        retire(
            &fold,
            gate.symbols[symbol_index],
            anchor_flowed,
            &mut totals.snapshots,
            &mut totals.updates,
            &mut totals.trades,
            &mut symbol_live,
        );
        totals.switches += 1;
        if symbol_due {
            (
                symbol_index,
                instrument,
                next_symbol_switch,
                next_timeframe_switch,
            ) = switch_symbol(&service, gate, generation, symbol_index, started, now);
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

    let anchor_now = live_count(anchor.as_ref());
    retire(
        &fold,
        gate.symbols[symbol_index],
        anchor_now > anchor_baseline,
        &mut totals.snapshots,
        &mut totals.updates,
        &mut totals.trades,
        &mut symbol_live,
    );
    service
        .shutdown(Duration::from_secs(10))
        .expect("engine shuts down");
    close_gate(gate, soak, started.elapsed(), &totals, &symbol_live);
}

/// Live candle tails plus order-flow trades one fold has seen.
fn live_count(fold: Option<&SeriesFold>) -> u64 {
    fold.map_or(0, |fold| fold.updates + fold.trades)
}

/// Reports what the soak did and records the gate's outcome.
fn close_gate(
    gate: &Gate,
    soak: Duration,
    elapsed: Duration,
    totals: &GateTotals,
    symbol_live: &BTreeMap<&'static str, u64>,
) {
    let GateTotals {
        switches,
        snapshots,
        updates,
        trades,
    } = *totals;
    eprintln!(
        "soaked {elapsed:?} on {}: {switches} switches, {snapshots} snapshots, {updates} updates, {trades} trades",
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
    // A subscription that never delivered in any window is a broken feed,
    // not a quiet venue: every gate symbol must have streamed at least one
    // live candle tail or order-flow trade somewhere in the soak.
    for symbol in gate.symbols {
        assert!(
            symbol_live.get(symbol).is_some_and(|live| *live > 0),
            "{symbol} never streamed a live publication in the whole soak"
        );
    }
    record_gate(
        gate.provider,
        GateOutcome::Passed,
        GateCompletion::Completed,
        &format!("{switches} switches, {snapshots} snapshots, {updates} updates, {trades} trades"),
    );
}
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
        // Rithmic selects one product set at a time: a second instrument
        // would fight the worker for the single session.
        anchor: None,
    });
}

/// Intervals the Rithmic chart cycles through.
const RITHMIC_TIMEFRAMES: [u32; 3] = [60, 300, 900];
/// Front-month contract roots the Rithmic gate switches between.
///
/// These are searched through the engine's own catalog, so a rolled contract
/// resolves to whatever the venue currently lists for the root.
const RITHMIC_SYMBOLS: [&str; 2] = ["MNQ", "MES"];

/// It needs no credentials: the public feed is unauthenticated, so this gate
/// runs on any networked runner:
///
/// ```text
/// cargo test -p axiusflow_engine --test live_market_soak -- --ignored --nocapture hyperliquid
/// ```
#[test]
#[ignore = "drives the live Hyperliquid public feed"]
fn live_hyperliquid_streams_across_markets_and_timeframe_switches() {
    run_gate(&Gate {
        provider: "hyperliquid",
        symbols: &HYPERLIQUID_SYMBOLS,
        timeframes: &HYPERLIQUID_TIMEFRAMES,
        anchor: Some("BTC"),
    });
}

/// Intervals the Hyperliquid chart cycles through.
const HYPERLIQUID_TIMEFRAMES: [u32; 3] = [60, 300, 900];
/// One market per supported category, resolved by exact wire symbol through
/// the engine's own catalog: core perpetual, spot pair, builder perpetual.
const HYPERLIQUID_SYMBOLS: [&str; 3] = ["BTC", "PURR/USDC", "xyz:TSLA"];
