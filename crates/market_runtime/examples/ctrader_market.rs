//! Exercises the cTrader provider through the ordinary `MarketService` catalog,
//! selection, history and live demand path against the stored DEMO connection.
//!
//! Never starts browser authorization and never touches the live host. When no
//! stored cTrader connection exists, or the broker connection needs a human
//! reconnect, the example prints a clear message and exits non-zero.
//!
//! Evidence is written to `.cache/evidence/ctrader_market_<timestamp>.json`
//! with the account id masked. Close the desktop first so provider session
//! concurrency limits are respected.
use aeris_contracts::{SearchProviderInstruments, SelectProviderInstrument, SeriesLoadState};
use aeris_ctrader_open_api_adapter::hosted::load_stored_connection;
use aeris_market_data::{BarPeriod, BarSeriesKey, MarketBar};
use aeris_market_engine::{MarketStream, StreamRequirements};
use aeris_market_runtime::{CtraderStreamStatistics, MarketRuntimeEvent, MarketService};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

const CLIENT: u64 = 1;
const WORKSPACE: u64 = 1;
const CONSUMER: u64 = 1;
const PROVIDER: &str = "ctrader";
const ENTITLEMENT: &str = "ctrader-authorized";
const DEMO_VENUE: &str = "cTrader Demo";
const SYMBOL: &str = "EURUSD";
const DEADLINE: Duration = Duration::from_secs(150);
const LIVE_WINDOW: Duration = Duration::from_secs(45);

/// One demand generation per timeframe so retired demands stay fenced.
const GENERATION_H1: u64 = 1;
const GENERATION_D1: u64 = 2;
const GENERATION_M1: u64 = 3;
const MINUTE_SECONDS: u32 = 60;

fn minute() -> Result<BarPeriod, String> {
    BarPeriod::time(MINUTE_SECONDS).map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Default)]
struct HistoryEvidence {
    bars: usize,
    contiguous: bool,
    session_gaps: usize,
    loaded: bool,
    elapsed_ms: u128,
}

impl HistoryEvidence {
    fn record(&mut self, bars: &[MarketBar], period: BarPeriod, started: Instant) {
        self.bars = bars.len();
        let mut gaps = 0;
        self.contiguous = bars_are_contiguous(bars, period, &mut gaps);
        self.session_gaps = gaps;
        self.loaded = true;
        self.elapsed_ms = started.elapsed().as_millis();
    }
}

const HOUR_NANOS: i64 = 3_600 * 1_000_000_000;

/// Time bars must be strictly ascending on the fixed interval grid; holes
/// aligned to the grid are venue session gaps, not corruption. Session days
/// open at 17:00 New York, so consecutive opens are 23 to 25 hours apart and
/// anything longer is a weekend or holiday.
fn bars_are_contiguous(bars: &[MarketBar], period: BarPeriod, session_gaps: &mut usize) -> bool {
    for pair in bars.windows(2) {
        let delta = pair[1].exchange_timestamp_unix_nanos - pair[0].exchange_timestamp_unix_nanos;
        let (aligned, next) = match period.duration_nanos() {
            Some(interval) => (delta > 0 && delta % interval == 0, interval),
            None => (delta >= 23 * HOUR_NANOS, 25 * HOUR_NANOS),
        };
        if !aligned {
            return false;
        }
        if delta > next {
            *session_gaps += 1;
        }
    }
    true
}

/// Masks the account id segment of `ctrader:<env>:<ctid>:<symbolId>`.
fn mask_instrument_id(instrument_id: &str) -> String {
    let segments: Vec<&str> = instrument_id.split(':').collect();
    if segments.len() != 4 {
        return "ctrader:redacted".to_string();
    }
    let ctid = segments[2];
    let suffix: String = ctid
        .chars()
        .rev()
        .take(2)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!(
        "{}:{}:#****{}:{}",
        segments[0], segments[1], suffix, segments[3]
    )
}

fn demand(
    market: &MarketService,
    instrument: &aeris_contracts::InstallProviderInstrument,
    generation: u64,
    period: BarPeriod,
    streams: StreamRequirements,
) -> Result<(), String> {
    let series = BarSeriesKey {
        provider_id: instrument.provider.clone(),
        instrument_id: instrument.instrument_id.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
        period,
        definition_version: 1,
    };
    market.set_demand(CLIENT, CONSUMER, generation, &series, streams)
}

#[derive(Default)]
struct QuoteEvidence {
    bbo_seen: bool,
    bbo_bid_below_ask: bool,
    depth_both_sides: bool,
}

#[derive(Default)]
struct LiveEvidence {
    candle_seen: bool,
    subscription_state_reached: bool,
    market_closed_fallback: bool,
}

#[derive(Default)]
struct Evidence {
    masked_instrument_id: Option<String>,
    m1: HistoryEvidence,
    h1: HistoryEvidence,
    d1: HistoryEvidence,
    quotes: QuoteEvidence,
    live: LiveEvidence,
    search_elapsed_ms: u128,
    selection_elapsed_ms: u128,
    live_window_ms: u128,
    total_elapsed_ms: u128,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stage {
    Search,
    HistoryH1,
    HistoryD1,
    HistoryM1,
    Live,
}

struct Driver<'a> {
    market: &'a MarketService,
    evidence: Evidence,
    instrument: Option<aeris_contracts::InstallProviderInstrument>,
    stage: Stage,
    stage_started: Instant,
    live_started: Option<Instant>,
}

impl<'a> Driver<'a> {
    fn new(market: &'a MarketService) -> Self {
        Self {
            market,
            evidence: Evidence::default(),
            instrument: None,
            stage: Stage::Search,
            stage_started: Instant::now(),
            live_started: None,
        }
    }

    fn handle(&mut self, event: MarketRuntimeEvent) -> Result<(), String> {
        match event {
            MarketRuntimeEvent::ProviderInstrumentSearchResult(result)
            | MarketRuntimeEvent::ProviderInstrumentSearchPreview(result) => {
                self.on_search(&result)
            }
            MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
                self.on_selection(selection.instrument)
            }
            MarketRuntimeEvent::SeriesSnapshot(publication) => self.on_snapshot(&publication),
            MarketRuntimeEvent::SeriesUpdate(update) => self.on_update(&update),
            MarketRuntimeEvent::OrderBookSnapshot(book) => {
                self.on_book(&book);
                Ok(())
            }
            MarketRuntimeEvent::SeriesState(series_state) => {
                if self.stage == Stage::Live
                    && matches!(
                        series_state.state,
                        SeriesLoadState::Ready | SeriesLoadState::Live
                    )
                {
                    self.evidence.live.subscription_state_reached = true;
                }
                Ok(())
            }
            MarketRuntimeEvent::ProviderState(provider_state) => {
                if provider_state.provider == PROVIDER {
                    println!(
                        "Provider: {:?} {:?}",
                        provider_state.state, provider_state.detail
                    );
                }
                Ok(())
            }
            MarketRuntimeEvent::ProviderCatalogRejected(rejection) => Err(format!(
                "cTrader catalog rejected the request ({:?}); the stored demo connection \
                 may need a human reconnect in the desktop",
                rejection.reason
            )),
            MarketRuntimeEvent::DemandError(error) => {
                Err(format!("Demand error {:?}: {}", error.code, error.detail))
            }
            _ => Ok(()),
        }
    }

    fn on_search(
        &mut self,
        result: &aeris_contracts::ProviderInstrumentSearchResult,
    ) -> Result<(), String> {
        if self.stage != Stage::Search || result.search_generation != 1 {
            return Ok(());
        }
        let Some(candidate) = result
            .instruments
            .iter()
            .find(|item| item.display_symbol == SYMBOL && item.exchange == DEMO_VENUE)
        else {
            return Ok(());
        };
        self.evidence.search_elapsed_ms = self.stage_started.elapsed().as_millis();
        self.stage_started = Instant::now();
        self.market.select_provider_instrument(
            CLIENT,
            SelectProviderInstrument {
                consumer_id: CONSUMER,
                selection_generation: 1,
                search_generation: 1,
                provider: PROVIDER.into(),
                symbol: candidate.symbol.clone(),
                exchange: candidate.exchange.clone(),
                entitlement_id: ENTITLEMENT.into(),
            },
        )
    }

    fn on_selection(
        &mut self,
        selected: aeris_contracts::InstallProviderInstrument,
    ) -> Result<(), String> {
        if self.stage != Stage::Search {
            return Ok(());
        }
        if !selected.instrument_id.starts_with("ctrader:demo:") {
            return Err(format!(
                "Selection resolved outside the demo host: {}",
                mask_instrument_id(&selected.instrument_id)
            ));
        }
        let masked = mask_instrument_id(&selected.instrument_id);
        println!("Selected {} ({masked})", selected.display_symbol);
        self.evidence.masked_instrument_id = Some(masked);
        self.evidence.selection_elapsed_ms = self.stage_started.elapsed().as_millis();
        demand(
            self.market,
            &selected,
            GENERATION_H1,
            BarPeriod::time(3_600).map_err(|error| error.to_string())?,
            StreamRequirements::BARS,
        )?;
        self.instrument = Some(selected);
        self.stage = Stage::HistoryH1;
        self.stage_started = Instant::now();
        Ok(())
    }

    fn on_snapshot(
        &mut self,
        publication: &aeris_market_engine::ConsumerPublication,
    ) -> Result<(), String> {
        let snapshot = &publication.snapshot;
        let interval = snapshot.series.period;
        let installed = self
            .instrument
            .as_ref()
            .ok_or("Snapshot arrived before instrument selection")?;
        match (self.stage, publication.generation.0.get()) {
            (Stage::HistoryH1, GENERATION_H1) => {
                self.evidence
                    .h1
                    .record(&snapshot.bars, interval, self.stage_started);
                Self::report_history("H1", &self.evidence.h1);
                demand(
                    self.market,
                    installed,
                    GENERATION_D1,
                    BarPeriod::Session { days: 1 },
                    StreamRequirements::BARS,
                )?;
                self.stage = Stage::HistoryD1;
                self.stage_started = Instant::now();
            }
            (Stage::HistoryD1, GENERATION_D1) => {
                self.evidence
                    .d1
                    .record(&snapshot.bars, interval, self.stage_started);
                Self::report_history("D1", &self.evidence.d1);
                demand(
                    self.market,
                    installed,
                    GENERATION_M1,
                    minute()?,
                    StreamRequirements::BARS
                        .with(MarketStream::Quotes)
                        .with(MarketStream::Depth),
                )?;
                self.stage = Stage::HistoryM1;
                self.stage_started = Instant::now();
            }
            (Stage::HistoryM1, GENERATION_M1) => {
                self.evidence
                    .m1
                    .record(&snapshot.bars, interval, self.stage_started);
                Self::report_history("M1", &self.evidence.m1);
                self.stage = Stage::Live;
                self.live_started = Some(Instant::now());
            }
            _ => {}
        }
        Ok(())
    }

    fn report_history(label: &str, check: &HistoryEvidence) {
        println!(
            "{label} history: {} bars (contiguous {}, {} session gaps)",
            check.bars, check.contiguous, check.session_gaps
        );
    }

    fn on_update(
        &mut self,
        update: &aeris_market_engine::ConsumerSeriesUpdate,
    ) -> Result<(), String> {
        if self.stage == Stage::Live && update.series.period == minute()? {
            self.evidence.live.candle_seen = true;
        }
        Ok(())
    }

    fn on_book(&mut self, book: &aeris_market_runtime::MarketOrderBookSnapshot) {
        if self.stage != Stage::Live {
            return;
        }
        if let (Some(bid), Some(ask)) = (book.publication.best_bid, book.publication.best_ask) {
            self.evidence.quotes.bbo_seen = true;
            if bid.price < ask.price {
                self.evidence.quotes.bbo_bid_below_ask = true;
            }
        }
        if !book.publication.bids.is_empty() && !book.publication.asks.is_empty() {
            self.evidence.quotes.depth_both_sides = true;
        }
    }

    fn live_finished(&mut self) -> bool {
        if self.stage != Stage::Live {
            return false;
        }
        let Some(from) = self.live_started else {
            return false;
        };
        self.evidence.live_window_ms = from.elapsed().as_millis();
        live_complete(&self.evidence) || from.elapsed() >= LIVE_WINDOW
    }
}

fn live_complete(evidence: &Evidence) -> bool {
    evidence.live.candle_seen
        && evidence.quotes.bbo_bid_below_ask
        && evidence.quotes.depth_both_sides
}

fn run() -> Result<(), String> {
    if load_stored_connection()?.is_none() {
        return Err(
            "No stored cTrader connection; connect the demo account through the human \
             authorization flow in the desktop first (this example never starts a browser \
             login and never uses the live host)"
                .to_string(),
        );
    }
    let started = Instant::now();
    let market = MarketService::start()?;
    let result = run_market(&market, started);
    let shutdown = market.shutdown(Duration::from_secs(10));
    result.and(shutdown)
}

fn run_market(market: &MarketService, started: Instant) -> Result<(), String> {
    market.attach(CLIENT)?;
    market.register_consumer(CLIENT, WORKSPACE, CONSUMER)?;
    let result = drive(market, started);
    let detach = market.detach(CLIENT);
    result.and(detach)
}

fn drive(market: &MarketService, started: Instant) -> Result<(), String> {
    let deadline = started + DEADLINE;
    let mut driver = Driver::new(market);
    market.search_provider_instruments(
        CLIENT,
        SearchProviderInstruments {
            consumer_id: CONSUMER,
            search_generation: 1,
            provider: PROVIDER.into(),
            query: SYMBOL.into(),
            maximum_results: 32,
            categories: aeris_contracts::InstrumentSearchCategories::ALL,
        },
    )?;
    while Instant::now() < deadline {
        while let Some(event) = market.poll_event(CLIENT, CONSUMER)? {
            driver.handle(event)?;
        }
        if driver.live_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    finish(market, driver.evidence, started)
}

fn finish(market: &MarketService, mut evidence: Evidence, started: Instant) -> Result<(), String> {
    evidence.total_elapsed_ms = started.elapsed().as_millis();
    // Live trendbar/spot subscriptions that were acked but produced no ticks
    // mean the venue session is closed; that is evidence, not failure.
    if !evidence.live.candle_seen
        && !evidence.quotes.bbo_seen
        && evidence.live.subscription_state_reached
    {
        evidence.live.market_closed_fallback = true;
    }
    let depth_status = if evidence.quotes.depth_both_sides {
        "both sides"
    } else if evidence.quotes.bbo_seen || evidence.live.subscription_state_reached {
        "no depth from broker"
    } else {
        "none"
    };
    let statistics = market.ctrader_stream_statistics();
    let path = write_evidence(&evidence, statistics, depth_status)?;
    let history_ok = [evidence.m1, evidence.h1, evidence.d1]
        .iter()
        .all(|check| check.loaded && check.contiguous);
    let live_ok = live_complete(&evidence)
        || (evidence.live.market_closed_fallback && evidence.live.subscription_state_reached);
    println!(
        "cTrader market: {} | M1 {} bars, H1 {} bars, D1 {} bars | live candle {} | \
         BBO {} (bid<ask {}) | depth {depth_status} | crossed skipped: {} spots, {} depth | \
         market-closed fallback {} | {} ms total",
        evidence
            .masked_instrument_id
            .as_deref()
            .unwrap_or("unresolved"),
        evidence.m1.bars,
        evidence.h1.bars,
        evidence.d1.bars,
        evidence.live.candle_seen,
        evidence.quotes.bbo_seen,
        evidence.quotes.bbo_bid_below_ask,
        statistics.crossed_spots,
        statistics.crossed_depth,
        evidence.live.market_closed_fallback,
        evidence.total_elapsed_ms,
    );
    println!("Evidence: {}", path.display());
    if !history_ok {
        return Err("cTrader history was missing or non-contiguous".to_string());
    }
    if !live_ok {
        return Err(
            "No live cTrader data arrived and the acked subscription state was not reached"
                .to_string(),
        );
    }
    Ok(())
}

fn write_evidence(
    evidence: &Evidence,
    statistics: CtraderStreamStatistics,
    depth_status: &str,
) -> Result<PathBuf, String> {
    let directory = Path::new(".cache/evidence");
    fs::create_dir_all(directory).map_err(|_| "Could not create evidence directory")?;
    let sequence = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "System clock is invalid")?
        .as_nanos();
    let path = directory.join(format!("ctrader_market_{sequence}.json"));
    let history = |check: &HistoryEvidence| {
        json!({
            "bars": check.bars,
            "contiguous": check.contiguous,
            "session_gaps": check.session_gaps,
            "elapsed_ms": check.elapsed_ms,
        })
    };
    let payload = json!({
        "provider": PROVIDER,
        "environment": "demo",
        "symbol": SYMBOL,
        "instrument_id": evidence.masked_instrument_id,
        "history": {
            "M1": history(&evidence.m1),
            "H1": history(&evidence.h1),
            "D1": history(&evidence.d1),
        },
        "live_candle_seen": evidence.live.candle_seen,
        "bbo_seen": evidence.quotes.bbo_seen,
        "bbo_bid_below_ask": evidence.quotes.bbo_bid_below_ask,
        "depth_status": depth_status,
        "subscription_state_reached": evidence.live.subscription_state_reached,
        "market_closed_fallback": evidence.live.market_closed_fallback,
        "crossed_skipped": {
            "spots": statistics.crossed_spots,
            "depth": statistics.crossed_depth,
        },
        "elapsed_ms": {
            "search": evidence.search_elapsed_ms,
            "selection": evidence.selection_elapsed_ms,
            "live_window": evidence.live_window_ms,
            "total": evidence.total_elapsed_ms,
        },
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| "Could not create market evidence")?;
    file.write_all(
        &serde_json::to_vec_pretty(&payload).map_err(|_| "Could not encode market evidence")?,
    )
    .map_err(|_| "Could not write market evidence")?;
    Ok(path)
}

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("{error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
