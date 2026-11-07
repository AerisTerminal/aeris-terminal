//! Hyperliquid market scan worker.
//!
//! One background thread evaluates the newest scan over the most traded listed markets the
//! catalog worker hands it. Bars come from public `candleSnapshot` requests at background
//! priority, so a long pass never spends the request budget charts and the catalog keep in
//! reserve, and from a bounded session cache that refreshes only each series' tail. The cache
//! lives in worker memory only. Publications share the catalog event channel, whose overflow
//! rejects every pending catalog command, so a pass publishes progress at most once per second.

use std::{
    cmp::Ordering as RankOrdering,
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use aeris_hyperliquid_market_adapter::{
    CandleSnapshotRequest, HYPERLIQUID_BUDGET_EXHAUSTED, HyperliquidCandlePage,
    HyperliquidHttpClient, HyperliquidHttpConfig, HyperliquidRequestPriority,
    NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE,
};
use aeris_market_data::MarketBar;

use crate::hyperliquid_realtime::HyperliquidCatalogEvent;
use crate::market_service::CatalogPublisher;
use crate::market_service::clock::Clock;
use crate::scan::{ScanBars, evaluate, price_decimals};
use crate::{
    MAXIMUM_SCAN_HISTORY_BARS, ProviderInstrumentSummary, ProviderMarketScan,
    ProviderMarketScanMatch, ScanDefinition, ScanProgress, ScanProviderMarkets, ScanTimeframe,
};

pub(crate) const SCAN_WORKER_NAME: &str = "aeris-hyperliquid-scan";
/// Scans waiting for the worker; it always runs the newest one.
pub(crate) const SCAN_JOB_CAPACITY: usize = 2;
/// Cached bars fetched this recently are evaluated again without a request.
const SCAN_BAR_REFRESH_AGE: Duration = Duration::from_secs(60);
/// Bars the session cache holds across every series.
const MAXIMUM_SCAN_CACHED_BARS: usize = 600_000;
/// Shortest gap between two in-progress publications of one pass.
const SCAN_PUBLICATION_INTERVAL: Duration = Duration::from_secs(1);
/// Wait before a budget-refused request is tried again.
const SCAN_BUDGET_RETRY: Duration = Duration::from_millis(500);
/// Longest idle wait before the worker observes cancellation.
const SCAN_IDLE_POLL: Duration = Duration::from_millis(250);
const NANOS_PER_MILLI: i64 = 1_000_000;

/// One scan with the universe the catalog resolved for it.
pub(crate) struct HyperliquidScanJob {
    pub(crate) request: ScanProviderMarkets,
    pub(crate) provider_generation: u64,
    /// Instruments to evaluate, most traded first, already bounded to the request.
    pub(crate) universe: Vec<ProviderInstrumentSummary>,
}

/// Runs the scan worker until cancellation or until the catalog worker hangs up.
pub(crate) fn run_scan<F>(
    jobs: &Receiver<HyperliquidScanJob>,
    events: &CatalogPublisher<HyperliquidCatalogEvent>,
    stop: &Arc<AtomicBool>,
    clock: &dyn Clock,
    fetch: F,
) where
    F: FnMut(
        &CandleSnapshotRequest<'_>,
        &Arc<AtomicBool>,
    ) -> Result<HyperliquidCandlePage, SeriesError>,
{
    ScanWorker {
        jobs,
        events,
        stop,
        clock,
        fetch,
        cache: ScanBarCache::new(MAXIMUM_SCAN_CACHED_BARS),
    }
    .run();
}

/// Fetches one candle page over HTTP, naming why a failed request failed.
pub(crate) fn fetch_candles(
    client: &mut HyperliquidHttpClient,
    request: &CandleSnapshotRequest<'_>,
    stop: &Arc<AtomicBool>,
) -> Result<HyperliquidCandlePage, SeriesError> {
    client
        .fetch_candle_snapshot(request, stop)
        .map_err(|error| classify_fetch_error(&error))
}

/// The adapter reports every failure as text; only its budget refusal is retried.
fn classify_fetch_error(error: &str) -> SeriesError {
    if error == HYPERLIQUID_BUDGET_EXHAUSTED {
        SeriesError::BudgetExhausted
    } else {
        SeriesError::Unavailable
    }
}

/// The newest queued job, discarding older ones.
fn newest_job(jobs: &Receiver<HyperliquidScanJob>) -> Option<HyperliquidScanJob> {
    let mut newest = None;
    while let Ok(job) = jobs.try_recv() {
        newest = Some(job);
    }
    newest
}

enum PassEnd {
    Finished,
    Superseded(HyperliquidScanJob),
    Stopped,
}

struct ScanWorker<'a, F> {
    jobs: &'a Receiver<HyperliquidScanJob>,
    events: &'a CatalogPublisher<HyperliquidCatalogEvent>,
    stop: &'a Arc<AtomicBool>,
    clock: &'a dyn Clock,
    fetch: F,
    cache: ScanBarCache,
}

impl<F> ScanWorker<'_, F>
where
    F: FnMut(
        &CandleSnapshotRequest<'_>,
        &Arc<AtomicBool>,
    ) -> Result<HyperliquidCandlePage, SeriesError>,
{
    fn run(&mut self) {
        let mut next = None;
        loop {
            if self.stopped() {
                return;
            }
            let job = match next.take() {
                Some(job) => job,
                None => match self.jobs.recv_timeout(SCAN_IDLE_POLL) {
                    Ok(job) => job,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            let job = newest_job(self.jobs).unwrap_or(job);
            match self.run_pass(&job) {
                PassEnd::Finished => {}
                PassEnd::Superseded(newer) => next = Some(newer),
                PassEnd::Stopped => return,
            }
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Evaluates every instrument of one job. A newer job ends the pass without another
    /// publication, so a consumer never sees an older scan after a newer one began.
    fn run_pass(&mut self, job: &HyperliquidScanJob) -> PassEnd {
        let definition = &job.request.definition;
        let depth = definition.history_bars();
        let mut progress = ScanProgress {
            evaluated: 0,
            universe: u32::try_from(job.universe.len()).unwrap_or(u32::MAX),
            unavailable: 0,
        };
        let mut matches = Vec::new();
        let mut published_at = self.clock.monotonic();
        let mut index = 0;
        while let Some(instrument) = job.universe.get(index) {
            if self.stopped() {
                return PassEnd::Stopped;
            }
            match self.cache.series_bars(
                &instrument.symbol,
                definition.timeframe,
                depth,
                self.clock,
                self.stop,
                &mut self.fetch,
            ) {
                Ok(bars) => {
                    progress.evaluated = progress.evaluated.saturating_add(1);
                    matches.extend(scan_match(definition, instrument, &bars));
                    index += 1;
                }
                Err(_) if self.stopped() => return PassEnd::Stopped,
                Err(SeriesError::BudgetExhausted) => {
                    match self.jobs.recv_timeout(SCAN_BUDGET_RETRY) {
                        Ok(newer) => {
                            return PassEnd::Superseded(newest_job(self.jobs).unwrap_or(newer));
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            std::thread::sleep(SCAN_BUDGET_RETRY);
                        }
                    }
                    continue;
                }
                Err(SeriesError::Unavailable) => {
                    progress.unavailable = progress.unavailable.saturating_add(1);
                    index += 1;
                }
            }
            if self.stopped() {
                return PassEnd::Stopped;
            }
            if let Some(newer) = newest_job(self.jobs) {
                return PassEnd::Superseded(newer);
            }
            let now = self.clock.monotonic();
            if index < job.universe.len()
                && now.saturating_duration_since(published_at) >= SCAN_PUBLICATION_INTERVAL
            {
                published_at = now;
                // An undelivered publication means the coordinator rejected the scan or is
                // gone, so the rest of the pass would spend budget for no consumer.
                if !self.publish(job, progress, false, &mut matches) {
                    return PassEnd::Finished;
                }
            }
        }
        self.publish(job, progress, true, &mut matches);
        PassEnd::Finished
    }

    /// Returns whether the publication reached the coordinator.
    fn publish(
        &self,
        job: &HyperliquidScanJob,
        progress: ScanProgress,
        complete: bool,
        matches: &mut [ProviderMarketScanMatch],
    ) -> bool {
        if self.stopped() {
            return false;
        }
        matches.sort_by(rank);
        let maximum_results = usize::try_from(job.request.maximum_results).unwrap_or(usize::MAX);
        let captured_at_unix_millis = self
            .clock
            .wall_nanos()
            .map_or(0, |nanos| nanos.div_euclid(NANOS_PER_MILLI));
        // The publisher reports overflow itself, and the coordinator then rejects the scan.
        self.events
            .send(HyperliquidCatalogEvent::ScanUpdated(ProviderMarketScan {
                consumer_id: job.request.consumer_id,
                provider: "hyperliquid".to_string(),
                provider_generation: job.provider_generation,
                scan_generation: job.request.scan_generation,
                progress,
                complete,
                captured_at_unix_millis,
                matches: matches.iter().take(maximum_results).cloned().collect(),
            }))
            .is_ok()
    }
}

/// Largest percent change first; an unknown change last; then by display symbol.
fn rank(left: &ProviderMarketScanMatch, right: &ProviderMarketScanMatch) -> RankOrdering {
    match (left.change_percent, right.change_percent) {
        (Some(left), Some(right)) => right.total_cmp(&left),
        (Some(_), None) => RankOrdering::Less,
        (None, Some(_)) => RankOrdering::Greater,
        (None, None) => RankOrdering::Equal,
    }
    .then_with(|| {
        left.instrument
            .display_symbol
            .cmp(&right.instrument.display_symbol)
    })
}

/// Evaluates one instrument's bars, the forming bar last.
fn scan_match(
    definition: &ScanDefinition,
    instrument: &ProviderInstrumentSummary,
    bars: &[MarketBar],
) -> Option<ProviderMarketScanMatch> {
    let current = bars.last()?;
    let price_scale = u8::try_from(NORMALIZED_PRICE_SCALE).ok()?;
    let quantity_scale = u8::try_from(NORMALIZED_QUANTITY_SCALE).ok()?;
    let outcome = evaluate(
        definition,
        &ScanBars::from_market_bars(bars, price_scale, quantity_scale),
    );
    outcome.matched.then(|| ProviderMarketScanMatch {
        instrument: instrument.clone(),
        price_scale: NORMALIZED_PRICE_SCALE,
        price_decimals: price_decimals(bars, NORMALIZED_PRICE_SCALE),
        close: current.close,
        quantity_scale: NORMALIZED_QUANTITY_SCALE,
        volume: current.volume,
        change_percent: outcome.change_percent,
        values: outcome.values,
        bar_open_unix_millis: current
            .exchange_timestamp_unix_nanos
            .div_euclid(NANOS_PER_MILLI),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SeriesError {
    /// The request budget refused the request; the same request succeeds later.
    BudgetExhausted,
    /// The provider or transport failed this request.
    Unavailable,
}

/// How a series is brought up to date before evaluation.
#[derive(Clone, Copy)]
enum Refresh {
    Reuse,
    Full,
    Tail { from_millis: i64 },
}

type SeriesKey = (String, ScanTimeframe);

/// Bars of recently scanned series, bounded by total bars with least-recently-used eviction.
struct ScanBarCache {
    series: BTreeMap<SeriesKey, CachedSeries>,
    total_bars: usize,
    maximum_bars: usize,
    uses: u64,
}

struct CachedSeries {
    /// Provider-closed bars in open-time order.
    closed: Vec<MarketBar>,
    forming: Option<MarketBar>,
    fetched_at: Instant,
    /// The last full fetch returned fewer bars than requested: the provider has no older ones.
    history_exhausted: bool,
    last_used: u64,
}

impl CachedSeries {
    fn len(&self) -> usize {
        self.closed.len() + usize::from(self.forming.is_some())
    }

    /// The newest `depth` bars, the forming bar last.
    fn window(&self, depth: usize) -> Vec<MarketBar> {
        let closed = depth.saturating_sub(usize::from(self.forming.is_some()));
        let start = self.closed.len().saturating_sub(closed);
        self.closed[start..]
            .iter()
            .copied()
            .chain(self.forming)
            .collect()
    }

    fn cap(&mut self) {
        let excess = self.len().saturating_sub(MAXIMUM_SCAN_HISTORY_BARS);
        self.closed.drain(..excess.min(self.closed.len()));
    }

    /// Replaces the cached bars from the first returned open time onward.
    fn merge_tail(&mut self, page: HyperliquidCandlePage) {
        let Some(first_open) = page
            .bars
            .first()
            .or(page.forming.as_ref())
            .map(|bar| bar.exchange_timestamp_unix_nanos)
        else {
            return;
        };
        let kept = self
            .closed
            .partition_point(|bar| bar.exchange_timestamp_unix_nanos < first_open);
        self.closed.truncate(kept);
        self.closed.extend(page.bars);
        self.forming = page.forming;
    }
}

impl ScanBarCache {
    fn new(maximum_bars: usize) -> Self {
        Self {
            series: BTreeMap::new(),
            total_bars: 0,
            maximum_bars,
            uses: 0,
        }
    }

    /// The newest `depth` bars of one series, fetching only what the cache cannot answer.
    fn series_bars<F>(
        &mut self,
        wire_coin: &str,
        timeframe: ScanTimeframe,
        depth: usize,
        clock: &dyn Clock,
        stop: &Arc<AtomicBool>,
        fetch: &mut F,
    ) -> Result<Vec<MarketBar>, SeriesError>
    where
        F: FnMut(
            &CandleSnapshotRequest<'_>,
            &Arc<AtomicBool>,
        ) -> Result<HyperliquidCandlePage, SeriesError>,
    {
        let key = (wire_coin.to_string(), timeframe);
        let now = clock.monotonic();
        let now_millis = clock
            .wall_nanos()
            .map(|nanos| nanos.div_euclid(NANOS_PER_MILLI))
            .ok_or(SeriesError::Unavailable)?;
        let refresh = plan_refresh(self.series.get(&key), timeframe, depth, now, now_millis);
        let page = match refresh {
            Refresh::Reuse => None,
            Refresh::Full => {
                let (start_millis, end_millis) = crate::hyperliquid_history::history_window(
                    timeframe.bar_period(),
                    depth,
                    now_millis,
                )
                .map_err(|_| SeriesError::Unavailable)?;
                Some(fetch_page(
                    wire_coin,
                    timeframe,
                    start_millis,
                    end_millis,
                    stop,
                    fetch,
                )?)
            }
            Refresh::Tail { from_millis } => Some(fetch_page(
                wire_coin,
                timeframe,
                from_millis,
                now_millis,
                stop,
                fetch,
            )?),
        };
        self.uses = self.uses.saturating_add(1);
        let previous = self.series.get(&key).map_or(0, CachedSeries::len);
        let series = match (refresh, page) {
            (Refresh::Full, Some(page)) => {
                let returned = page.bars.len() + usize::from(page.forming.is_some());
                self.series.insert(
                    key.clone(),
                    CachedSeries {
                        closed: page.bars,
                        forming: page.forming,
                        fetched_at: now,
                        history_exhausted: returned < depth,
                        last_used: self.uses,
                    },
                );
                self.series.get_mut(&key)
            }
            (Refresh::Tail { .. }, Some(page)) => {
                if let Some(series) = self.series.get_mut(&key) {
                    series.merge_tail(page);
                    series.fetched_at = now;
                }
                self.series.get_mut(&key)
            }
            _ => self.series.get_mut(&key),
        }
        .ok_or(SeriesError::Unavailable)?;
        series.cap();
        series.last_used = self.uses;
        let window = series.window(depth);
        let current = series.len();
        self.total_bars = self.total_bars.saturating_sub(previous) + current;
        self.evict_beyond_bound(&key);
        Ok(window)
    }

    fn evict_beyond_bound(&mut self, keep: &SeriesKey) {
        while self.total_bars > self.maximum_bars {
            let Some(oldest) = self
                .series
                .iter()
                .filter(|(key, _)| *key != keep)
                .min_by_key(|(_, series)| series.last_used)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            if let Some(evicted) = self.series.remove(&oldest) {
                self.total_bars = self.total_bars.saturating_sub(evicted.len());
            }
        }
    }
}

fn plan_refresh(
    cached: Option<&CachedSeries>,
    timeframe: ScanTimeframe,
    depth: usize,
    now: Instant,
    now_millis: i64,
) -> Refresh {
    let Some(cached) = cached else {
        return Refresh::Full;
    };
    if cached.len() < depth && !cached.history_exhausted {
        return Refresh::Full;
    }
    if now.saturating_duration_since(cached.fetched_at) < SCAN_BAR_REFRESH_AGE {
        return Refresh::Reuse;
    }
    let Some(last_closed) = cached.closed.last() else {
        return Refresh::Full;
    };
    let from_millis = last_closed
        .exchange_timestamp_unix_nanos
        .div_euclid(NANOS_PER_MILLI);
    // A tail longer than a full window costs the same request and could pass one page.
    let tail_bars = now_millis.saturating_sub(from_millis) / bar_millis(timeframe) + 1;
    if from_millis >= now_millis || !usize::try_from(tail_bars).is_ok_and(|bars| bars <= depth) {
        return Refresh::Full;
    }
    Refresh::Tail { from_millis }
}

const fn bar_millis(timeframe: ScanTimeframe) -> i64 {
    match timeframe {
        ScanTimeframe::Minutes15 => 900_000,
        ScanTimeframe::Hour1 => 3_600_000,
        ScanTimeframe::Hour4 => 14_400_000,
        ScanTimeframe::Day1 => 86_400_000,
        ScanTimeframe::Week1 => 604_800_000,
    }
}

fn fetch_page<F>(
    wire_coin: &str,
    timeframe: ScanTimeframe,
    start_millis: i64,
    end_millis: i64,
    stop: &Arc<AtomicBool>,
    fetch: &mut F,
) -> Result<HyperliquidCandlePage, SeriesError>
where
    F: FnMut(
        &CandleSnapshotRequest<'_>,
        &Arc<AtomicBool>,
    ) -> Result<HyperliquidCandlePage, SeriesError>,
{
    fetch(
        &CandleSnapshotRequest {
            wire_coin,
            period: timeframe.bar_period(),
            start_millis,
            end_millis,
            now_millis: end_millis,
            price_scale: NORMALIZED_PRICE_SCALE,
            quantity_scale: NORMALIZED_QUANTITY_SCALE,
            config: HyperliquidHttpConfig::default(),
            priority: HyperliquidRequestPriority::Background,
        },
        stop,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_service::ProviderCoordinatorWake;
    use crate::market_service::clock::ScriptedClock;
    use crate::{
        ProviderInstrumentKind, ScanComparison, ScanCondition, ScanConditionValues, ScanMatchMode,
        ScanNumber, ScanOperand, ScanPriceField,
    };
    use std::sync::mpsc::{self, SyncSender};

    const DAY_MILLIS: i64 = 86_400_000;
    /// Day 20,000 of the epoch plus one hour, so the current daily bar is forming.
    const NOW_MILLIS: i64 = 20_000 * DAY_MILLIS + 3_600_000;

    struct Request {
        coin: String,
        start_millis: i64,
        end_millis: i64,
        priority: HyperliquidRequestPriority,
    }

    fn price(units: i64) -> i64 {
        units * 10_i64.pow(NORMALIZED_PRICE_SCALE)
    }

    fn bar(day: i64, close: i64) -> MarketBar {
        let open_millis = day * DAY_MILLIS;
        MarketBar {
            source_sequence: 0,
            exchange_timestamp_seconds: open_millis / 1_000,
            exchange_timestamp_unix_nanos: open_millis * NANOS_PER_MILLI,
            open: price(close),
            high: price(close),
            low: price(close),
            close: price(close),
            volume: price(7),
        }
    }

    /// Closed bars on the days before `today`, then the forming bar of `today`.
    fn page(today: i64, closes: &[i64]) -> HyperliquidCandlePage {
        let (forming, history) = closes.split_last().expect("at least one bar");
        let first = today - i64::try_from(history.len()).expect("bar count");
        HyperliquidCandlePage {
            bars: history
                .iter()
                .enumerate()
                .map(|(offset, close)| bar(first + i64::try_from(offset).expect("offset"), *close))
                .collect(),
            forming: Some(bar(today, *forming)),
            handoff_close_millis: Some(today * DAY_MILLIS),
        }
    }

    fn instrument(symbol: &str) -> ProviderInstrumentSummary {
        ProviderInstrumentSummary {
            kind: ProviderInstrumentKind::Instrument,
            symbol: symbol.to_string(),
            display_symbol: format!("{symbol}-USD"),
            exchange: "core".to_string(),
            ..ProviderInstrumentSummary::default()
        }
    }

    /// Close above 100 on the daily timeframe.
    fn close_above_100() -> ScanDefinition {
        ScanDefinition {
            timeframe: ScanTimeframe::Day1,
            match_mode: ScanMatchMode::All,
            conditions: vec![ScanCondition {
                left: ScanOperand::Price(ScanPriceField::Close),
                comparison: ScanComparison::Above,
                right: ScanOperand::Number(ScanNumber {
                    units: 100,
                    scale: 0,
                }),
                upper: None,
            }],
        }
    }

    fn job(generation: u64, symbols: &[&str], maximum_results: u32) -> HyperliquidScanJob {
        HyperliquidScanJob {
            request: ScanProviderMarkets {
                consumer_id: 3,
                scan_generation: generation,
                provider: "hyperliquid".to_string(),
                definition: close_above_100(),
                universe_limit: 64,
                maximum_results,
            },
            provider_generation: 7,
            universe: symbols.iter().map(|symbol| instrument(symbol)).collect(),
        }
    }

    fn scripted_clock() -> Arc<ScriptedClock> {
        Arc::new(ScriptedClock::new(NOW_MILLIS * NANOS_PER_MILLI))
    }

    /// Runs the worker over `jobs` until the job channel hangs up and returns every scan
    /// publication. `sender` receives the job channel so a fetch can queue a newer job.
    fn run_worker<F>(
        jobs: Vec<HyperliquidScanJob>,
        clock: &ScriptedClock,
        fetch: impl FnOnce(SyncSender<HyperliquidScanJob>) -> F,
    ) -> Vec<ProviderMarketScan>
    where
        F: FnMut(
            &CandleSnapshotRequest<'_>,
            &Arc<AtomicBool>,
        ) -> Result<HyperliquidCandlePage, SeriesError>,
    {
        let (job_sender, job_receiver) = mpsc::sync_channel(SCAN_JOB_CAPACITY);
        for job in jobs {
            job_sender.try_send(job).expect("queued job fits");
        }
        let fetch = fetch(job_sender);
        let (event_sender, event_receiver) = mpsc::sync_channel(64);
        let events = CatalogPublisher::new(
            event_sender,
            "hyperliquid",
            ProviderCoordinatorWake::for_tests(),
        );
        run_scan(
            &job_receiver,
            &events,
            &Arc::new(AtomicBool::new(false)),
            clock,
            fetch,
        );
        event_receiver
            .try_iter()
            .map(|event| match event {
                HyperliquidCatalogEvent::ScanUpdated(scan) => scan,
                _ => panic!("the scan worker publishes only scan updates"),
            })
            .collect()
    }

    fn symbols(scan: &ProviderMarketScan) -> Vec<&str> {
        scan.matches
            .iter()
            .map(|found| found.instrument.symbol.as_str())
            .collect()
    }

    #[test]
    fn a_pass_matches_ranks_and_bounds_its_publications() {
        let clock = scripted_clock();
        let requests = std::cell::RefCell::new(Vec::new());
        let log = &requests;
        let fetch_clock = Arc::clone(&clock);
        let scans = run_worker(
            vec![job(5, &["AAA", "BBB", "CCC", "DDD"], 2)],
            &clock,
            move |sender| {
                drop(sender);
                move |request: &CandleSnapshotRequest<'_>,
                      _: &Arc<AtomicBool>|
                      -> Result<HyperliquidCandlePage, SeriesError> {
                    log.borrow_mut().push(Request {
                        coin: request.wire_coin.to_string(),
                        start_millis: request.start_millis,
                        end_millis: request.end_millis,
                        priority: request.priority,
                    });
                    fetch_clock.advance(Duration::from_millis(600));
                    let close = match request.wire_coin {
                        "AAA" => 150,
                        "BBB" => 120,
                        "CCC" => 90,
                        _ => 300,
                    };
                    Ok(page(20_000, &[100, close]))
                }
            },
        );

        let requests = requests.into_inner();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.coin.as_str())
                .collect::<Vec<_>>(),
            ["AAA", "BBB", "CCC", "DDD"]
        );
        for (fetched_before, request) in (0_i64..).zip(&requests) {
            // Each scripted fetch advances the clock, so every window ends at its own now.
            let (start, end) = crate::hyperliquid_history::history_window(
                ScanTimeframe::Day1.bar_period(),
                close_above_100().history_bars(),
                NOW_MILLIS + fetched_before * 600,
            )
            .expect("window");
            assert_eq!(request.priority, HyperliquidRequestPriority::Background);
            assert_eq!((request.start_millis, request.end_millis), (start, end));
        }

        assert_eq!(scans.len(), 2, "one progress publication and one final");
        let progress = &scans[0];
        assert!(!progress.complete);
        assert_eq!(progress.scan_generation, 5);
        assert_eq!(
            progress.progress,
            ScanProgress {
                evaluated: 2,
                universe: 4,
                unavailable: 0,
            }
        );
        assert_eq!(symbols(progress), ["AAA", "BBB"]);

        let last = &scans[1];
        assert!(last.complete);
        assert_eq!(last.consumer_id, 3);
        assert_eq!(last.provider, "hyperliquid");
        assert_eq!(last.provider_generation, 7);
        assert_eq!(last.progress.evaluated, 4);
        assert_eq!(
            symbols(last),
            ["DDD", "AAA"],
            "ordered by change and bounded"
        );
        let best = &last.matches[0];
        assert_eq!(best.close, price(300));
        assert_eq!(best.volume, price(7));
        assert_eq!(best.price_scale, NORMALIZED_PRICE_SCALE);
        assert_eq!(best.price_decimals, 0, "whole-number fixture prices");
        assert_eq!(
            best.values,
            vec![ScanConditionValues {
                left: Some(300.0),
                right: Some(100.0),
            }]
        );
        assert!((best.change_percent.expect("change") - 200.0).abs() < 1e-9);
        assert_eq!(best.bar_open_unix_millis, 20_000 * DAY_MILLIS);
        assert_eq!(
            last.captured_at_unix_millis,
            NOW_MILLIS + 4 * 600,
            "captured on the injected wall clock"
        );
    }

    #[test]
    fn a_newer_scan_supersedes_the_running_pass() {
        let clock = scripted_clock();
        let fetched = std::cell::RefCell::new(Vec::new());
        let log = &fetched;
        let fetch_clock = Arc::clone(&clock);
        let scans = run_worker(
            vec![job(1, &["AAA", "BBB", "CCC"], 10)],
            &clock,
            move |sender| {
                let mut sender = Some(sender);
                move |request: &CandleSnapshotRequest<'_>,
                      _: &Arc<AtomicBool>|
                      -> Result<HyperliquidCandlePage, SeriesError> {
                    log.borrow_mut().push(request.wire_coin.to_string());
                    fetch_clock.advance(Duration::from_secs(2));
                    if let Some(sender) = sender.take() {
                        sender
                            .try_send(job(2, &["DDD"], 10))
                            .expect("newer job fits");
                    }
                    Ok(page(20_000, &[100, 200]))
                }
            },
        );

        assert_eq!(fetched.into_inner(), ["AAA", "DDD"]);
        assert_eq!(scans.len(), 1, "the superseded pass publishes nothing more");
        assert_eq!(scans[0].scan_generation, 2);
        assert!(scans[0].complete);
        assert_eq!(symbols(&scans[0]), ["DDD"]);
    }

    #[test]
    fn fetch_failures_count_as_unavailable_and_the_pass_continues() {
        let clock = scripted_clock();
        let attempts = std::cell::Cell::new(0);
        let counter = &attempts;
        let scans = run_worker(
            vec![job(1, &["AAA", "BBB", "CCC"], 10)],
            &clock,
            move |sender| {
                drop(sender);
                move |request: &CandleSnapshotRequest<'_>,
                      _: &Arc<AtomicBool>|
                      -> Result<HyperliquidCandlePage, SeriesError> {
                    counter.set(counter.get() + 1);
                    match request.wire_coin {
                        "AAA" => Err(SeriesError::Unavailable),
                        // The budget refuses the first request; the retry is not unavailable.
                        "BBB" if counter.get() == 2 => Err(SeriesError::BudgetExhausted),
                        _ => Ok(page(20_000, &[100, 200])),
                    }
                }
            },
        );

        assert_eq!(attempts.get(), 4);
        assert_eq!(scans.len(), 1);
        assert_eq!(
            scans[0].progress,
            ScanProgress {
                evaluated: 2,
                universe: 3,
                unavailable: 1,
            }
        );
        assert_eq!(symbols(&scans[0]), ["BBB", "CCC"]);
    }

    fn cached_closes(cache: &ScanBarCache, coin: &str) -> Vec<i64> {
        let series = &cache.series[&(coin.to_string(), ScanTimeframe::Day1)];
        series
            .closed
            .iter()
            .chain(series.forming.as_ref())
            .map(|bar| bar.close / price(1))
            .collect()
    }

    #[test]
    fn cached_series_are_reused_then_refresh_only_their_tail() {
        let clock = scripted_clock();
        let stop = Arc::new(AtomicBool::new(false));
        let mut cache = ScanBarCache::new(MAXIMUM_SCAN_CACHED_BARS);
        let mut requests = Vec::new();
        let mut pages = vec![page(20_001, &[11, 12, 13]), page(20_000, &[1, 2, 3])];
        let mut fetch = |request: &CandleSnapshotRequest<'_>,
                         _: &Arc<AtomicBool>|
         -> Result<HyperliquidCandlePage, SeriesError> {
            requests.push((request.start_millis, request.end_millis));
            Ok(pages.pop().expect("scripted page"))
        };

        let bars = cache
            .series_bars("AAA", ScanTimeframe::Day1, 3, &*clock, &stop, &mut fetch)
            .expect("first fetch");
        assert_eq!(bars.len(), 3);
        clock.advance(Duration::from_secs(59));
        let reused = cache
            .series_bars("AAA", ScanTimeframe::Day1, 3, &*clock, &stop, &mut fetch)
            .expect("cached bars");
        assert_eq!(reused, bars, "a series fetched within a minute is reused");

        // A day later the cached series is stale: only its tail is requested, from the open
        // of its last closed bar, and the returned bars replace the cached ones from there.
        clock.advance(Duration::from_secs(86_400 - 59));
        let refreshed = cache
            .series_bars("AAA", ScanTimeframe::Day1, 3, &*clock, &stop, &mut fetch)
            .expect("tail refresh");
        drop(fetch);
        let now = NOW_MILLIS + DAY_MILLIS;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1], (19_999 * DAY_MILLIS, now));
        assert_eq!(cached_closes(&cache, "AAA"), [1, 11, 12, 13]);
        assert_eq!(
            refreshed
                .iter()
                .map(|bar| bar.close / price(1))
                .collect::<Vec<_>>(),
            [11, 12, 13]
        );
        assert_eq!(
            refreshed[2].exchange_timestamp_unix_nanos,
            20_001 * DAY_MILLIS * NANOS_PER_MILLI
        );
        assert_eq!(cache.total_bars, 4);
    }

    #[test]
    fn a_deeper_scan_refetches_a_short_series_the_provider_can_extend() {
        let clock = scripted_clock();
        let stop = Arc::new(AtomicBool::new(false));
        let mut cache = ScanBarCache::new(MAXIMUM_SCAN_CACHED_BARS);
        let mut calls = 0;
        let mut fetch = |_: &CandleSnapshotRequest<'_>,
                         _: &Arc<AtomicBool>|
         -> Result<HyperliquidCandlePage, SeriesError> {
            calls += 1;
            Ok(page(20_000, &[1, 2, 3]))
        };
        cache
            .series_bars("AAA", ScanTimeframe::Day1, 3, &*clock, &stop, &mut fetch)
            .expect("shallow");
        cache
            .series_bars("AAA", ScanTimeframe::Day1, 5, &*clock, &stop, &mut fetch)
            .expect("deeper refetch");
        // The deeper fetch returned fewer bars than requested: the listing has no more.
        cache
            .series_bars("AAA", ScanTimeframe::Day1, 5, &*clock, &stop, &mut fetch)
            .expect("exhausted history is reused");
        drop(fetch);
        assert_eq!(calls, 2);
    }

    #[test]
    fn the_cache_evicts_the_least_recently_used_series_beyond_its_bound() {
        let clock = scripted_clock();
        let stop = Arc::new(AtomicBool::new(false));
        let mut cache = ScanBarCache::new(10);
        let mut fetched = Vec::new();
        let mut fetch = |request: &CandleSnapshotRequest<'_>,
                         _: &Arc<AtomicBool>|
         -> Result<HyperliquidCandlePage, SeriesError> {
            fetched.push(request.wire_coin.to_string());
            Ok(page(20_000, &[1, 2, 3, 4]))
        };
        for coin in ["AAA", "BBB", "AAA", "CCC", "BBB"] {
            cache
                .series_bars(coin, ScanTimeframe::Day1, 4, &*clock, &stop, &mut fetch)
                .expect("bars");
        }
        drop(fetch);
        assert_eq!(fetched, ["AAA", "BBB", "CCC", "BBB"]);
        assert_eq!(
            cache
                .series
                .keys()
                .map(|(coin, _)| coin.as_str())
                .collect::<Vec<_>>(),
            ["BBB", "CCC"],
            "AAA was least recently used when BBB returned"
        );
        assert_eq!(cache.total_bars, 8);
    }

    #[test]
    fn a_series_never_holds_more_than_the_scan_history_bound() {
        let mut series = CachedSeries {
            closed: (0..3_100).map(|day| bar(day, 1)).collect(),
            forming: Some(bar(3_100, 1)),
            fetched_at: Instant::now(),
            history_exhausted: false,
            last_used: 0,
        };
        series.cap();
        assert_eq!(series.len(), MAXIMUM_SCAN_HISTORY_BARS);
        assert_eq!(
            series.closed[0].exchange_timestamp_unix_nanos,
            101 * DAY_MILLIS * NANOS_PER_MILLI
        );
    }

    #[test]
    fn only_a_budget_refusal_is_retried() {
        assert_eq!(
            classify_fetch_error(HYPERLIQUID_BUDGET_EXHAUSTED),
            SeriesError::BudgetExhausted
        );
        assert_eq!(
            classify_fetch_error("hyperliquid info request failed: HTTP 500"),
            SeriesError::Unavailable
        );
    }
}
