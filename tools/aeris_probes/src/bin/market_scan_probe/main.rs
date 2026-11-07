//! Market-scan probe: runs real scans through `MarketService` against the live public
//! Hyperliquid feed on an isolated validation root. It measures a cold and a warm pass over
//! the most traded markets on one interval (`--interval`, daily by default), opens a chart on
//! that interval while the cold pass runs to show background scan requests leave charts their
//! budget reserve, checks one match's values against the bars that chart received, and checks
//! that an interval the provider does not offer is refused. Records an `aeris.probe.v1` envelope and exits non-zero on
//! the first failed check. Non-shipping validation tooling.

use std::{
    error::Error,
    path::PathBuf,
    time::{Duration, Instant},
};

use aeris_market_data::{ChartInterval, MarketBar};
use aeris_market_runtime::{
    InstrumentRef, InstrumentRequest, MarketRuntimeEvent, MarketService, ProviderMarketScan,
    ScanComparison, ScanCondition, ScanDefinition, ScanIndicator, ScanNumber, ScanOperand,
    ScanPriceField, ScanProviderMarkets, SelectionOutcome, SelectionRequest,
    SeriesPresentationStatus, SeriesTailOperation, StreamRequirements,
    built_in_provider_presentations, persisted_interval,
};
use aeris_platform_runtime::data_root::{DataRoot, InstanceLock};
use aeris_probes::{Envelope, Evidence, ProbeError, ProbeResult, market, validate_paths};
use serde_json::json;

const CLIENT: u64 = 81;
const SCAN_CONSUMER: u64 = 82;
const CHART_CONSUMER: u64 = 83;
const UNIVERSE: u32 = 100;
/// The chart opens once the cold pass has evaluated this many markets.
const CHART_AFTER_EVALUATED: u32 = 5;
const CHART_DEADLINE: Duration = Duration::from_secs(30);
/// A chart that waits longer than this behind scan traffic has lost its reserve.
const CHART_LATENCY_LIMIT: Duration = Duration::from_secs(15);
const PASS_DEADLINE: Duration = Duration::from_mins(12);
const SCAN_ATTEMPTS: u32 = 6;
const SCAN_RETRY_DELAY: Duration = Duration::from_secs(5);
const MAXIMUM_CHART_BARS: usize = 20_000;
const SPOT_CHECK_SYMBOL: &str = "ETH";
const SPOT_PERIOD: u16 = 20;

/// An interval no provider offers for scans, which every scan request must refuse.
const UNOFFERED_INTERVAL: ChartInterval = ChartInterval::Second5;

struct Options {
    root: PathBuf,
    out: PathBuf,
    interval: ChartInterval,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, &'static str> {
        let mut root = None;
        let mut out = None;
        let mut interval = None;
        let mut arguments = arguments.into_iter();
        while let Some(flag) = arguments.next() {
            let value = |arguments: &mut dyn Iterator<Item = String>| {
                arguments.next().ok_or("missing probe option value")
            };
            match flag.as_str() {
                "--validation-root" if root.is_none() => {
                    root = Some(PathBuf::from(value(&mut arguments)?));
                }
                "--out" if out.is_none() => out = Some(PathBuf::from(value(&mut arguments)?)),
                "--interval" if interval.is_none() => {
                    interval = Some(
                        persisted_interval(&value(&mut arguments)?)
                            .ok_or("--interval is not a chart interval label")?,
                    );
                }
                _ => return Err("unknown, duplicate or unsafe probe option"),
            }
        }
        let root = root.ok_or("--validation-root is required")?;
        let out = out.ok_or("--out is required")?;
        validate_paths(&root, &out)?;
        Ok(Self {
            root,
            out,
            interval: interval.unwrap_or(ChartInterval::Day1),
        })
    }
}

#[derive(Default)]
struct Chart {
    generation: u64,
    selected: bool,
    live: bool,
    price_scale: u8,
    bars: Vec<MarketBar>,
}

impl Chart {
    fn ready(&self) -> bool {
        self.selected && self.live && self.bars.len() > 10
    }

    fn observe(&mut self, event: MarketRuntimeEvent) -> ProbeResult<()> {
        match event {
            MarketRuntimeEvent::SelectionOutcome(SelectionOutcome::Selected {
                generation, ..
            }) if generation.0.get() == self.generation => self.selected = true,
            MarketRuntimeEvent::SelectionOutcome(SelectionOutcome::Failed {
                generation,
                error,
            }) if generation.0.get() == self.generation => {
                return Err(ProbeError::Runtime(format!(
                    "chart selection failed: {error:?}"
                )));
            }
            MarketRuntimeEvent::SeriesState(state)
                if state.generation.0.get() == self.generation =>
            {
                self.live = matches!(state.presentation, Some(SeriesPresentationStatus::Live));
            }
            MarketRuntimeEvent::SeriesSnapshot(publication)
                if publication.generation.0.get() == self.generation =>
            {
                let snapshot = &publication.snapshot;
                if snapshot.bars.len() > MAXIMUM_CHART_BARS {
                    return Err(ProbeError::Invariant(
                        "chart snapshot exceeds probe bar capacity".to_string(),
                    ));
                }
                self.price_scale = snapshot.price_scale;
                self.bars = snapshot.bars.to_vec();
            }
            MarketRuntimeEvent::SeriesUpdate(update)
                if update.generation.0.get() == self.generation =>
            {
                match update.operation {
                    SeriesTailOperation::Revise => {
                        if let Some(last) = self.bars.last_mut()
                            && last.exchange_timestamp_unix_nanos
                                == update.bar.exchange_timestamp_unix_nanos
                        {
                            *last = update.bar;
                        }
                    }
                    SeriesTailOperation::Append => {
                        if self.bars.len() < MAXIMUM_CHART_BARS
                            && self.bars.last().is_none_or(|bar| {
                                bar.exchange_timestamp_unix_nanos
                                    < update.bar.exchange_timestamp_unix_nanos
                            })
                        {
                            self.bars.push(update.bar);
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// One scan pass as the probe saw it: every publication's progress, then the final answer.
struct Pass {
    elapsed: Duration,
    progress: Vec<(u128, u32, u32)>,
    last: ProviderMarketScan,
}

struct Probe {
    service: MarketService,
    provider: &'static str,
    interval: ChartInterval,
    chart: Chart,
    scan_generation: u64,
    scan: Option<ProviderMarketScan>,
    scan_rejected: bool,
}

impl Probe {
    fn pump(&mut self) -> ProbeResult<()> {
        if !self
            .service
            .wait_for_events(CLIENT, Duration::from_millis(50))
        {
            return Ok(());
        }
        for (consumer, event) in market(
            self.service
                .drain_events(CLIENT, &[(SCAN_CONSUMER, 256), (CHART_CONSUMER, 256)]),
        )? {
            match (consumer, event.value) {
                (SCAN_CONSUMER, MarketRuntimeEvent::ProviderMarketScan(scan))
                    if scan.scan_generation == self.scan_generation =>
                {
                    self.scan = Some(scan);
                }
                (SCAN_CONSUMER, MarketRuntimeEvent::ProviderMarketScanRejected(rejection))
                    if rejection.command_generation == self.scan_generation =>
                {
                    self.scan_rejected = true;
                }
                (CHART_CONSUMER, event) => self.chart.observe(event)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn open_chart(&mut self, symbol: &str) -> ProbeResult<Instant> {
        self.chart = Chart {
            generation: self.chart.generation + 1,
            ..Chart::default()
        };
        let started = Instant::now();
        market(self.service.select_and_demand(
            CLIENT,
            SelectionRequest {
                consumer_id: CHART_CONSUMER,
                generation: self.chart.generation,
                instrument: InstrumentRequest::Ref(InstrumentRef {
                    provider_id: self.provider.into(),
                    instrument_id: format!("{}:perp:{symbol}", self.provider),
                    entitlement_id: format!("{}-public", self.provider),
                    query: Some(symbol.into()),
                }),
                interval: self.interval,
                streams: StreamRequirements::BARS,
                viewport: None,
            },
        ))?;
        Ok(started)
    }

    fn wait_for_chart(&mut self, started: Instant) -> ProbeResult<Duration> {
        while !self.chart.ready() {
            if started.elapsed() > CHART_DEADLINE {
                return Err(ProbeError::Invariant(
                    "chart history did not load in time".to_string(),
                ));
            }
            self.pump()?;
        }
        Ok(started.elapsed())
    }

    fn scan_request(&mut self, interval: ChartInterval) -> ScanProviderMarkets {
        self.scan_generation += 1;
        self.scan = None;
        self.scan_rejected = false;
        ScanProviderMarkets {
            consumer_id: SCAN_CONSUMER,
            scan_generation: self.scan_generation,
            provider: self.provider.to_string(),
            definition: definition(interval),
            universe_limit: UNIVERSE,
            maximum_results: aeris_market_runtime::MAXIMUM_SCAN_RESULTS,
        }
    }

    fn send_scan(&mut self) -> ProbeResult<()> {
        let request = self.scan_request(self.interval);
        market(self.service.scan_provider_markets(CLIENT, request))
    }

    /// Whether the runtime refuses a scan on an interval the provider does not offer.
    fn unoffered_interval_refused(&mut self) -> Result<String, ()> {
        let request = self.scan_request(UNOFFERED_INTERVAL);
        match self.service.scan_provider_markets(CLIENT, request) {
            Ok(()) => Err(()),
            Err(error) => Ok(error),
        }
    }

    /// Runs one pass to completion; `chart_symbol` opens a chart partway through it and
    /// returns how long its history took.
    fn run_pass(&mut self, chart_symbol: Option<&str>) -> ProbeResult<(Pass, Option<Duration>)> {
        let mut attempts = 0;
        let started = loop {
            attempts += 1;
            self.send_scan()?;
            let started = Instant::now();
            while self.scan.is_none() && !self.scan_rejected {
                if started.elapsed() > PASS_DEADLINE {
                    return Err(ProbeError::Invariant("scan never answered".to_string()));
                }
                self.pump()?;
            }
            if !self.scan_rejected {
                break started;
            }
            if attempts == SCAN_ATTEMPTS {
                return Err(ProbeError::Invariant(
                    "scan was rejected on every attempt".to_string(),
                ));
            }
            std::thread::sleep(SCAN_RETRY_DELAY);
        };
        let mut progress = Vec::new();
        let mut seen = None;
        let mut chart_started = None;
        let mut chart_latency = None;
        loop {
            if let Some(scan) = &self.scan
                && seen
                    != Some((
                        scan.progress.evaluated,
                        scan.progress.unavailable,
                        scan.complete,
                    ))
            {
                seen = Some((
                    scan.progress.evaluated,
                    scan.progress.unavailable,
                    scan.complete,
                ));
                progress.push((
                    started.elapsed().as_millis(),
                    scan.progress.evaluated,
                    scan.progress.unavailable,
                ));
                if scan.complete {
                    break;
                }
            }
            if self.scan_rejected {
                return Err(ProbeError::Invariant(
                    "scan was rejected mid-pass".to_string(),
                ));
            }
            if started.elapsed() > PASS_DEADLINE {
                return Err(ProbeError::Invariant(
                    "scan pass did not complete in time".to_string(),
                ));
            }
            let evaluated = self.scan.as_ref().map_or(0, |scan| scan.progress.evaluated);
            if let Some(symbol) = chart_symbol
                && chart_started.is_none()
                && evaluated >= CHART_AFTER_EVALUATED
            {
                chart_started = Some(self.open_chart(symbol)?);
            }
            if let Some(opened) = chart_started
                && chart_latency.is_none()
                && self.chart.ready()
            {
                chart_latency = Some(opened.elapsed());
            }
            if let Some(opened) = chart_started
                && chart_latency.is_none()
                && opened.elapsed() > CHART_DEADLINE
            {
                return Err(ProbeError::Invariant(
                    "chart history did not load during the scan".to_string(),
                ));
            }
            self.pump()?;
        }
        let last = self
            .scan
            .clone()
            .ok_or_else(|| ProbeError::Invariant("scan answer missing".to_string()))?;
        Ok((
            Pass {
                elapsed: started.elapsed(),
                progress,
                last,
            },
            chart_latency,
        ))
    }
}

fn number(units: i64) -> ScanOperand {
    ScanOperand::Number(ScanNumber { units, scale: 0 })
}

/// Every condition holds for any listed market with enough history, so every market with
/// data is a match and its values can be checked.
fn definition(interval: ChartInterval) -> ScanDefinition {
    let above_zero = |left| ScanCondition {
        left,
        comparison: ScanComparison::Above,
        right: number(0),
        upper: None,
    };
    ScanDefinition {
        timeframe: interval,
        conditions: vec![
            above_zero(ScanOperand::Price(ScanPriceField::Close)),
            above_zero(ScanOperand::Indicator(ScanIndicator::HighestHigh {
                period: SPOT_PERIOD,
            })),
            above_zero(ScanOperand::Indicator(ScanIndicator::Sma {
                period: SPOT_PERIOD,
            })),
            ScanCondition {
                left: ScanOperand::Indicator(ScanIndicator::Rsi { period: 14 }),
                comparison: ScanComparison::Between,
                right: number(0),
                upper: Some(number(100)),
            },
        ],
        ..ScanDefinition::default()
    }
}

#[allow(clippy::cast_precision_loss)]
fn scaled(units: i64, scale: u8) -> f64 {
    units as f64 / 10_f64.powi(i32::from(scale))
}

fn pass_metrics(pass: &Pass) -> serde_json::Value {
    let scanned = pass.last.progress.evaluated + pass.last.progress.unavailable;
    let minutes = pass.elapsed.as_secs_f64() / 60.0;
    json!({
        "elapsed_ms": pass.elapsed.as_millis(),
        "universe": pass.last.progress.universe,
        "evaluated": pass.last.progress.evaluated,
        "unavailable": pass.last.progress.unavailable,
        "matches": pass.last.matches.len(),
        "markets_per_minute": if minutes > 0.0 { f64::from(scanned) / minutes } else { 0.0 },
        "publications": pass.progress.len(),
        "progress": pass.progress.iter().map(|(ms, evaluated, unavailable)| {
            json!({"ms": ms, "evaluated": evaluated, "unavailable": unavailable})
        }).collect::<Vec<_>>(),
    })
}

fn check_pass(evidence: &mut Evidence, id: &'static str, pass: &Pass) -> ProbeResult<()> {
    let progress = pass.last.progress;
    let ordered = pass
        .progress
        .windows(2)
        .all(|pair| pair[0].1 <= pair[1].1 && pair[0].2 <= pair[1].2);
    evidence.check(
        id,
        pass.last.complete
            && progress.universe > 0
            && progress.universe <= UNIVERSE
            && progress.evaluated + progress.unavailable == progress.universe
            && progress.unavailable <= progress.universe / 10
            && ordered
            && !pass.last.matches.is_empty(),
        pass_metrics(pass),
    )
}

fn spot_check(
    probe: &Probe,
    scan: &ProviderMarketScan,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let found = scan
        .matches
        .iter()
        .find(|row| row.instrument.symbol == SPOT_CHECK_SYMBOL);
    let bars = &probe.chart.bars;
    let period = usize::from(SPOT_PERIOD);
    let (Some(row), true) = (found, bars.len() > period) else {
        return evidence.check(
            "spot_check_matches_chart",
            false,
            json!({"symbol": SPOT_CHECK_SYMBOL, "found": found.is_some(), "chart_bars": bars.len()}),
        );
    };
    let scale = probe.chart.price_scale;
    let current = bars[bars.len() - 1];
    let prior = &bars[bars.len() - 1 - period..bars.len() - 1];
    let chart_highest = prior
        .iter()
        .map(|bar| scaled(bar.high, scale))
        .fold(f64::MIN, f64::max);
    let chart_sma = bars[bars.len() - period..]
        .iter()
        .map(|bar| scaled(bar.close, scale))
        .sum::<f64>()
        / f64::from(SPOT_PERIOD);
    let scan_highest = row.values.get(1).and_then(|values| values.left);
    let scan_sma = row.values.get(2).and_then(|values| values.left);
    let same_bar = current.exchange_timestamp_unix_nanos / 1_000_000 == row.bar_open_unix_millis;
    // The highest high excludes the current bar, so closed bars must agree exactly; the
    // average includes the forming bar, whose close moves between the two reads.
    let highest_agrees =
        scan_highest.is_some_and(|value| (value - chart_highest).abs() <= chart_highest * 1e-12);
    let sma_agrees =
        scan_sma.is_some_and(|value| (value - chart_sma).abs() <= chart_sma.abs() * 0.002);
    evidence.check(
        "spot_check_matches_chart",
        same_bar && highest_agrees && sma_agrees,
        json!({
            "symbol": SPOT_CHECK_SYMBOL,
            "same_current_bar": same_bar,
            "scan_bar_open_ms": row.bar_open_unix_millis,
            "chart_bar_open_ms": current.exchange_timestamp_unix_nanos / 1_000_000,
            "highest_high_20": {"scan": scan_highest, "chart": chart_highest},
            "sma_20": {"scan": scan_sma, "chart": chart_sma},
            "scan_close": scaled(row.close, u8::try_from(row.price_scale).unwrap_or(u8::MAX)),
            "chart_close": scaled(current.close, scale),
        }),
    )
}

fn scenario(
    service: &MarketService,
    interval: ChartInterval,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    let provider = built_in_provider_presentations()
        .iter()
        .find(|presentation| presentation.market_scan_available)
        .map(|presentation| presentation.id);
    evidence.check(
        "scan_provider_published",
        provider.is_some(),
        json!({"provider": provider}),
    )?;
    let provider = provider.unwrap_or_default();
    market(service.attach(CLIENT))?;
    market(service.register_consumer(CLIENT, 1, SCAN_CONSUMER))?;
    market(service.register_consumer(CLIENT, 1, CHART_CONSUMER))?;
    let mut probe = Probe {
        service: service.clone(),
        provider,
        interval,
        chart: Chart::default(),
        scan_generation: 0,
        scan: None,
        scan_rejected: false,
    };

    let started = probe.open_chart("BTC")?;
    let baseline = probe.wait_for_chart(started)?;
    evidence.check(
        "baseline_chart_history",
        baseline <= CHART_LATENCY_LIMIT,
        json!({"symbol": "BTC", "interval": interval.label(), "latency_ms": baseline.as_millis()}),
    )?;
    let refusal = probe.unoffered_interval_refused();
    evidence.check(
        "unoffered_interval_refused",
        refusal.is_ok(),
        json!({"interval": UNOFFERED_INTERVAL.label(), "error": refusal.ok()}),
    )?;

    let (cold, chart_latency) = probe.run_pass(Some(SPOT_CHECK_SYMBOL))?;
    evidence.check(
        "cold_pass_progressive",
        cold.progress
            .iter()
            .filter(|(_, evaluated, _)| *evaluated > 0)
            .count()
            >= 2,
        json!({"publications": cold.progress.len()}),
    )?;
    check_pass(evidence, "cold_pass_complete", &cold)?;
    evidence.check(
        "chart_history_during_scan",
        chart_latency.is_some_and(|latency| latency <= CHART_LATENCY_LIMIT),
        json!({
            "symbol": SPOT_CHECK_SYMBOL,
            "interval": interval.label(),
            "latency_ms": chart_latency.map(|latency| latency.as_millis()),
            "baseline_ms": baseline.as_millis(),
        }),
    )?;

    let (warm, _) = probe.run_pass(None)?;
    check_pass(evidence, "warm_pass_complete", &warm)?;
    evidence.metrics = json!({
        "interval": interval.label(),
        "cold_pass": pass_metrics(&cold),
        "warm_pass": pass_metrics(&warm),
    });
    spot_check(&probe, &warm.last, evidence)?;
    market(service.detach(CLIENT))
}

fn run(root: &DataRoot, interval: ChartInterval, evidence: &mut Evidence) -> ProbeResult<()> {
    let _lock =
        InstanceLock::acquire(root).map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let service = market(MarketService::start())?;
    let result = scenario(&service, interval, evidence);
    let shutdown = service.shutdown(Duration::from_secs(10));
    result?;
    market(shutdown)
}

fn main() -> Result<(), Box<dyn Error>> {
    use aeris_platform_runtime::data_root::{DataRootMode, initialize_data_root};

    let options = Options::parse(std::env::args().skip(1)).map_err(ProbeError::Argument)?;
    // Reject unsafe roots before collecting evidence or attempting to write output.
    let root = initialize_data_root(DataRootMode::Validation {
        root: options.root.clone(),
    })
    .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let envelope = Envelope::collect("market_scan", options.root.clone(), options.out.clone())?;
    let started_at = aeris_probes::utc_now().to_rfc3339();
    let mut evidence = Evidence::new();
    let result = run(root, options.interval, &mut evidence);
    let passed = envelope.finish(&started_at, &mut evidence, &result)?;
    if !passed {
        return Err(Box::new(ProbeError::Invariant(
            "market scan probe failed; see JSON evidence".to_string(),
        )));
    }
    println!("Market scan probe passed: {}", options.out.display());
    Ok(())
}
