//! Exercises ordinary `MarketService` catalog, selection, history and live demand.
//! Close the desktop first. Credentials remain in the native vault and hosted broker service.
//! Git Bash rewrites `/ES` into a Windows path; run from PowerShell or set
//! `MSYS_NO_PATHCONV=1` before invoking this example.
use aeris_contracts::{
    InstallProviderInstrument, ProviderConnectionState, SearchProviderInstruments,
    SelectProviderInstrument,
};
use aeris_market_data::{BarPeriod, BarSeriesKey};
use aeris_market_engine::{MarketStream, StreamRequirements};
use aeris_market_runtime::{MarketRuntimeEvent, MarketService, MarketTradeTapeSnapshot};
use std::{
    collections::VecDeque,
    thread,
    time::{Duration, Instant},
};
fn run(market: &MarketService) -> Result<(), String> {
    let query = probe_query();
    market.attach(1)?;
    market.register_consumer(1, 1, 1)?;
    let mut search_timings = SearchTimings::new();
    market.search_provider_instruments(
        1,
        SearchProviderInstruments {
            consumer_id: 1,
            search_generation: 1,
            provider: "tastytrade".into(),
            query: query.clone(),
            maximum_results: 100,
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut selected = false;
    let mut installed_instrument = None;
    let mut selection_started: Option<Instant> = None;
    let mut snapshots = 0;
    let mut tails = 0;
    let mut trades = 0;
    let mut history_check = HistoryCheck::default();
    let mut live_timing = LiveTiming::default();
    while Instant::now() < deadline {
        if let Some(event) = market.poll_event(1, 1)? {
            match event {
                MarketRuntimeEvent::ProviderInstrumentSearchResult(result) => {
                    selection_started = search_timings
                        .accept(market, result, &query, true)?
                        .or(selection_started);
                }
                MarketRuntimeEvent::ProviderInstrumentSearchPreview(result) => {
                    selection_started = search_timings
                        .accept(market, result, &query, false)?
                        .or(selection_started);
                }
                MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
                    installed_instrument = Some(selection.instrument.clone());
                    demand(market, 1, selection.instrument)?;
                    selected = true;
                }
                MarketRuntimeEvent::SeriesSnapshot(snapshot) => {
                    if snapshots == 0
                        && let Some(started) = selection_started
                    {
                        println!(
                            "Timing: selection to first candles {} ms",
                            started.elapsed().as_millis()
                        );
                    }
                    snapshots += 1;
                    history_check.accept(market, &snapshot)?;
                }
                MarketRuntimeEvent::SeriesUpdate(_) => {
                    tails += 1;
                    live_timing.record_update();
                }
                MarketRuntimeEvent::OrderBookSnapshot(_) => {
                    live_timing.record_order_book();
                }
                MarketRuntimeEvent::TradeTapeSnapshot(tape) => {
                    report_tape(&tape, &mut trades);
                    live_timing.record_tape();
                }
                MarketRuntimeEvent::ProviderState(state) => {
                    println!("Provider: {:?} {:?}", state.state, state.detail);
                }
                MarketRuntimeEvent::DemandError(error) => println!("Demand error: {error:?}"),
                MarketRuntimeEvent::ProviderCatalogRejected(error) => {
                    return Err(format!("Catalog rejected: {:?}", error.reason));
                }
                MarketRuntimeEvent::SeriesState(state) => {
                    println!("Series: {:?} {:?}", state.state, state.detail);
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(5));
        }
        if cycle_complete(&history_check, tails, trades) {
            break;
        }
    }
    finish_run(
        market,
        RunOutcome {
            selected,
            snapshots,
            tails,
            trades,
            history_check,
            installed_instrument,
            live_timing,
        },
    )?;
    market.detach(1)?;
    Ok(())
}

fn probe_query() -> String {
    std::env::args()
        .nth(1)
        .filter(|arg| arg != "--cycle")
        .or_else(|| std::env::args().nth(2))
        .unwrap_or_else(|| "/ES".into())
}

fn cycle_complete(history: &HistoryCheck, tails: usize, trades: usize) -> bool {
    history.switched && tails >= 2 && trades > 0
}

struct RunOutcome {
    selected: bool,
    snapshots: usize,
    tails: usize,
    trades: usize,
    history_check: HistoryCheck,
    installed_instrument: Option<InstallProviderInstrument>,
    live_timing: LiveTiming,
}

fn finish_run(market: &MarketService, outcome: RunOutcome) -> Result<(), String> {
    println!(
        "Selected={}; snapshots={}; updates={}; ticks={}",
        outcome.selected, outcome.snapshots, outcome.tails, outcome.trades
    );
    outcome.live_timing.report();
    if !outcome.selected
        || outcome.snapshots == 0
        || outcome.tails == 0
        || !outcome.history_check.switched
    {
        return Err("Catalog/chart/live integration did not complete".into());
    }
    let old_generation = outcome
        .history_check
        .provider_generation
        .ok_or("Initial tastytrade generation missing")?;
    measure_idle_reconnect(
        market,
        outcome
            .installed_instrument
            .ok_or("Selected instrument missing")?,
        old_generation,
    )
}

const LIVE_TIMING_SAMPLE_CAPACITY: usize = 4096;

#[derive(Default)]
struct LiveTiming {
    updates: IntervalSamples,
    order_books: IntervalSamples,
    tapes: IntervalSamples,
}

impl LiveTiming {
    fn record_update(&mut self) {
        self.updates.record();
    }

    fn record_order_book(&mut self) {
        self.order_books.record();
    }

    fn record_tape(&mut self) {
        self.tapes.record();
    }

    fn report(&self) {
        self.updates.report("chart update");
        self.order_books.report("order-book/quote publication");
        self.tapes.report("tape snapshot");
    }
}

#[derive(Default)]
struct IntervalSamples {
    previous: Option<Instant>,
    intervals_ms: VecDeque<u128>,
}

impl IntervalSamples {
    fn record(&mut self) {
        let now = Instant::now();
        if let Some(previous) = self.previous.replace(now) {
            if self.intervals_ms.len() == LIVE_TIMING_SAMPLE_CAPACITY {
                self.intervals_ms.pop_front();
            }
            self.intervals_ms
                .push_back(now.duration_since(previous).as_millis());
        }
    }

    fn report(&self, label: &str) {
        if self.intervals_ms.is_empty() {
            println!("Timing: {label} intervals unavailable (fewer than two events)");
            return;
        }
        let mut values: Vec<_> = self.intervals_ms.iter().copied().collect();
        values.sort_unstable();
        println!(
            "Timing: {label} interval p50={} ms p90={} ms p99={} ms samples={}",
            percentile(&values, 50),
            percentile(&values, 90),
            percentile(&values, 99),
            values.len()
        );
    }
}

fn percentile(values: &[u128], percentile: usize) -> u128 {
    let index = (values.len() - 1) * percentile / 100;
    values[index]
}

#[cfg(test)]
mod tests {
    use super::{IntervalSamples, LIVE_TIMING_SAMPLE_CAPACITY, percentile};

    #[test]
    fn interval_samples_are_bounded() {
        let mut samples = IntervalSamples::default();
        for _ in 0..=LIVE_TIMING_SAMPLE_CAPACITY {
            samples.record();
        }
        assert_eq!(samples.intervals_ms.len(), LIVE_TIMING_SAMPLE_CAPACITY);
    }

    #[test]
    fn percentile_uses_nearest_lower_rank() {
        assert_eq!(percentile(&[10, 20, 30, 40], 50), 20);
        assert_eq!(percentile(&[10, 20, 30, 40], 99), 30);
    }
}

fn report_tape(tape: &MarketTradeTapeSnapshot, trades: &mut usize) {
    if *trades == 0 || tape.trades.len() >= *trades + 1024 {
        *trades = tape.trades.len();
        let first = tape
            .trades
            .iter()
            .filter_map(|trade| trade.trade.metadata.timestamps.exchange_unix_nanos)
            .min();
        let last = tape
            .trades
            .iter()
            .filter_map(|trade| trade.trade.metadata.timestamps.exchange_unix_nanos)
            .max();
        println!("Retained ticks={trades}; first exchange nanoseconds={first:?}; last={last:?}");
    }
}
fn measure_idle_reconnect(
    market: &MarketService,
    instrument: InstallProviderInstrument,
    old_generation: u64,
) -> Result<(), String> {
    market.register_consumer(1, 1, 2)?;
    market.remove_consumer(1, 1)?;
    let stop_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() >= stop_deadline {
            let states: Vec<_> = market
                .status()?
                .providers
                .iter()
                .filter(|state| state.provider == "tastytrade")
                .map(|state| (state.state, state.generation))
                .collect();
            return Err(format!(
                "Tastytrade idle stop did not close the session: {states:?}"
            ));
        }
        if market.status()?.providers.iter().any(|state| {
            state.provider == "tastytrade" && state.state == ProviderConnectionState::Disconnected
        }) {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let reconnect_started = Instant::now();
    demand(market, 2, instrument)?;
    let reconnect_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() >= reconnect_deadline {
            return Err("Tastytrade did not return to live after idle stop".into());
        }
        if let Some(MarketRuntimeEvent::ProviderState(state)) = market.poll_event(1, 2)?
            && state.provider == "tastytrade"
            && state.state == ProviderConnectionState::Online
            && state.generation > old_generation
        {
            println!(
                "Timing: idle stop to live again {} ms",
                reconnect_started.elapsed().as_millis()
            );
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
struct SearchTimings {
    cold: Instant,
    cold_done: bool,
    warm: Option<Instant>,
    equity: Option<Instant>,
}
impl SearchTimings {
    fn new() -> Self {
        Self {
            cold: Instant::now(),
            cold_done: false,
            warm: None,
            equity: None,
        }
    }
    fn accept(
        &mut self,
        market: &MarketService,
        result: aeris_contracts::ProviderInstrumentSearchResult,
        query: &str,
        final_result: bool,
    ) -> Result<Option<Instant>, String> {
        match result.search_generation {
            1 => {
                if self.cold_done {
                    return Ok(None);
                }
                let contains_requested = result.instruments.iter().any(|item| {
                    item.symbol == query
                        || (matches!(query, "ES" | "/ES")
                            && item.symbol.starts_with("/ES")
                            && item.instrument_type.as_deref() == Some("Future"))
                });
                if !final_result && !contains_requested {
                    return Ok(None);
                }
                self.cold_done = true;
                println!(
                    "Timing: search to first results {} ms",
                    self.cold.elapsed().as_millis()
                );
                let selection_started = Instant::now();
                select(market, result, query)?;
                self.warm = Some(Instant::now());
                market.search_provider_instruments(
                    1,
                    SearchProviderInstruments {
                        consumer_id: 1,
                        search_generation: 2,
                        provider: "tastytrade".into(),
                        query: "/ES".into(),
                        maximum_results: 100,
                    },
                )?;
                Ok(Some(selection_started))
            }
            2 => {
                let Some(started) = self.warm.take() else {
                    return Ok(None);
                };
                println!(
                    "Timing: cached futures search to results {} ms",
                    started.elapsed().as_millis()
                );
                self.equity = Some(Instant::now());
                market.search_provider_instruments(
                    1,
                    SearchProviderInstruments {
                        consumer_id: 1,
                        search_generation: 3,
                        provider: "tastytrade".into(),
                        query: "SPY".into(),
                        maximum_results: 100,
                    },
                )?;
                Ok(None)
            }
            3 => {
                if !result.instruments.is_empty()
                    && let Some(started) = self.equity.take()
                {
                    println!(
                        "Timing: remote equity search to results {} ms",
                        started.elapsed().as_millis()
                    );
                }
                Ok(None)
            }
            _ => Err("Unexpected catalog measurement generation".into()),
        }
    }
}
#[derive(Default)]
struct HistoryCheck {
    initial_first: Option<i64>,
    provider_generation: Option<u64>,
    backfilled: bool,
    switched: bool,
    switch_started: Option<Instant>,
}
impl HistoryCheck {
    fn accept(
        &mut self,
        market: &MarketService,
        publication: &aeris_market_engine::ConsumerPublication,
    ) -> Result<(), String> {
        let snapshot = &publication.snapshot;
        let first = snapshot
            .bars
            .first()
            .ok_or("Empty historical chart")?
            .exchange_timestamp_unix_nanos;
        let generation = snapshot.provider_generation.0.get();
        println!(
            "History: {} bars; period {:?}; generation {generation}; first {}",
            snapshot.bars.len(),
            snapshot.series.period,
            first / 1_000_000_000
        );
        if self
            .provider_generation
            .is_some_and(|prior| prior != generation)
        {
            return Err(
                "A healthy symbol/timeframe/history change recreated the provider session".into(),
            );
        }
        self.provider_generation = Some(generation);
        match self.initial_first {
            None => {
                self.initial_first = Some(first);
                market.set_viewport(
                    1,
                    1,
                    1,
                    first - 1000 * 60 * 1_000_000_000,
                    first + 60 * 1_000_000_000,
                )?;
            }
            Some(initial) if !self.backfilled && first < initial => {
                self.backfilled = true;
                println!("Older viewport history loaded on the same session");
                let mut series = snapshot.series.clone();
                series.period = BarPeriod::time(300).map_err(|e| e.to_string())?;
                self.switch_started = Some(Instant::now());
                market.set_demand(
                    1,
                    1,
                    2,
                    &series,
                    StreamRequirements::BARS
                        .with(MarketStream::Trades)
                        .with(MarketStream::Quotes),
                )?;
            }
            _ if self.backfilled
                && snapshot.series.period == BarPeriod::time(300).map_err(|e| e.to_string())? =>
            {
                self.switched = true;
                if let Some(started) = self.switch_started {
                    println!(
                        "Timing: timeframe switch to candles {} ms",
                        started.elapsed().as_millis()
                    );
                }
                println!("Five-minute candles loaded on the same session");
            }
            _ => {}
        }
        Ok(())
    }
}
fn main() -> Result<(), String> {
    let market = MarketService::start()?;
    let result = if std::env::args().nth(1).as_deref() == Some("--history-load") {
        run_history_load(&market)
    } else if std::env::args().nth(1).as_deref() == Some("--cycle") {
        run_timeframe_cycle(&market)
    } else {
        run(&market)
    };
    let shutdown = market.shutdown(Duration::from_secs(10));
    result.and(shutdown)
}

fn run_timeframe_cycle(market: &MarketService) -> Result<(), String> {
    let query = std::env::args().nth(2).unwrap_or_else(|| "/ES".into());
    let fast = std::env::args().any(|arg| arg == "--fast");
    market.attach(1)?;
    market.register_consumer(1, 1, 1)?;
    market.search_provider_instruments(
        1,
        SearchProviderInstruments {
            consumer_id: 1,
            search_generation: 1,
            provider: "tastytrade".into(),
            query: query.clone(),
            maximum_results: 100,
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut instrument = None;
    while instrument.is_none() && Instant::now() < deadline {
        if let Some(event) = market.poll_event(1, 1)? {
            match event {
                MarketRuntimeEvent::ProviderInstrumentSearchResult(result) => {
                    select(market, result, &query)?;
                }
                MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
                    instrument = Some(selection.instrument);
                }
                MarketRuntimeEvent::ProviderState(state) => println!(
                    "Provider transition: {:?} detail={:?} generation={:?}",
                    state.state, state.detail, state.generation
                ),
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(5));
        }
    }
    let instrument = instrument.ok_or("cycle selection timed out")?;
    let periods = [60_u32, 300, 900, 3_600];
    let delay = if fast { 200 } else { 1_500 };
    for cycle in 0..12 {
        let seconds = periods[cycle % periods.len()];
        let series = BarSeriesKey {
            provider_id: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: instrument.entitlement_id.clone(),
            period: BarPeriod::time(seconds).map_err(|e| e.to_string())?,
            definition_version: 1,
        };
        println!("Cycle {} timeframe={}ms", cycle + 1, seconds * 1_000);
        market.set_demand(
            1,
            1,
            u64::try_from(cycle + 2).map_err(|_| "cycle generation overflow")?,
            &series,
            StreamRequirements::BARS
                .with(MarketStream::Trades)
                .with(MarketStream::Quotes),
        )?;
        let until = Instant::now() + Duration::from_millis(delay);
        while Instant::now() < until {
            if let Some(event) = market.poll_event(1, 1)? {
                match event {
                    MarketRuntimeEvent::ProviderState(state) => println!(
                        "Provider transition: {:?} detail={:?} generation={:?}",
                        state.state, state.detail, state.generation
                    ),
                    MarketRuntimeEvent::SeriesSnapshot(snapshot) => println!(
                        "First candles timeframe={} generation={} bars={}",
                        seconds,
                        snapshot.snapshot.provider_generation.0.get(),
                        snapshot.snapshot.bars.len()
                    ),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    settle_cycle(market)?;
    market.detach(1)
}

fn settle_cycle(market: &MarketService) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let (mut updates, mut order_books, mut tapes, mut snapshots) = (0, 0, 0, 0);
    while Instant::now() < deadline {
        if let Some(event) = market.poll_event(1, 1)? {
            match event {
                MarketRuntimeEvent::ProviderState(state) => println!(
                    "Settle provider transition: {:?} detail={:?} generation={:?}",
                    state.state, state.detail, state.generation
                ),
                MarketRuntimeEvent::SeriesSnapshot(snapshot) => {
                    snapshots += 1;
                    println!(
                        "Settle snapshot: generation={} bars={}",
                        snapshot.snapshot.provider_generation.0.get(),
                        snapshot.snapshot.bars.len()
                    );
                }
                MarketRuntimeEvent::SeriesUpdate(_) => updates += 1,
                MarketRuntimeEvent::OrderBookSnapshot(_) => order_books += 1,
                MarketRuntimeEvent::TradeTapeSnapshot(_) => tapes += 1,
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(5));
        }
    }
    println!(
        "Settle counts: SeriesSnapshot={snapshots}; SeriesUpdate={updates}; \
         OrderBookSnapshot={order_books}; TradeTapeSnapshot={tapes}"
    );
    Ok(())
}

fn run_history_load(market: &MarketService) -> Result<(), String> {
    let periods = [60, 300, 900, 3_600];
    market.attach(1)?;
    for consumer in 1..=periods.len() {
        let consumer = u64::try_from(consumer).map_err(|_| "Consumer identity overflow")?;
        market.register_consumer(1, 1, consumer)?;
    }
    market.search_provider_instruments(
        1,
        SearchProviderInstruments {
            consumer_id: 1,
            search_generation: 1,
            provider: "tastytrade".into(),
            query: "/ES".into(),
            maximum_results: 100,
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut selecting = false;
    let mut started = None;
    let mut snapshots = [false; 4];
    while Instant::now() < deadline {
        for consumer in 1..=periods.len() {
            let consumer = u64::try_from(consumer).map_err(|_| "Consumer identity overflow")?;
            let Some(event) = market.poll_event(1, consumer)? else {
                continue;
            };
            match event {
                MarketRuntimeEvent::ProviderInstrumentSearchResult(result)
                | MarketRuntimeEvent::ProviderInstrumentSearchPreview(result)
                    if !selecting =>
                {
                    selecting = true;
                    select_load_instrument(market, result)?;
                }
                MarketRuntimeEvent::ProviderInstrumentSelection(selection) if started.is_none() => {
                    started = Some(Instant::now());
                    for (index, seconds) in periods.iter().copied().enumerate() {
                        let series = BarSeriesKey {
                            provider_id: selection.instrument.provider.clone(),
                            instrument_id: selection.instrument.instrument_id.clone(),
                            entitlement_id: selection.instrument.entitlement_id.clone(),
                            period: BarPeriod::time(seconds).map_err(|error| error.to_string())?,
                            definition_version: 1,
                        };
                        market.set_demand(
                            1,
                            u64::try_from(index + 1).map_err(|_| "Consumer identity overflow")?,
                            1,
                            &series,
                            StreamRequirements::BARS,
                        )?;
                    }
                }
                MarketRuntimeEvent::SeriesSnapshot(publication) => {
                    let Some(index) = periods.iter().position(|seconds| {
                        BarPeriod::time(*seconds)
                            .is_ok_and(|period| publication.snapshot.series.period == period)
                    }) else {
                        return Err("Unexpected history load period".into());
                    };
                    if !snapshots[index] {
                        snapshots[index] = true;
                        println!(
                            "History load: {}-second candles visible at {} ms ({} bars)",
                            periods[index],
                            started
                                .ok_or("History load start missing")?
                                .elapsed()
                                .as_millis(),
                            publication.snapshot.bars.len()
                        );
                    }
                }
                MarketRuntimeEvent::DemandError(error) => {
                    return Err(format!("History load demand failed: {error:?}"));
                }
                _ => {}
            }
        }
        if snapshots.iter().all(|ready| *ready) {
            market.detach(1)?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(5));
    }
    Err(format!("History load timed out: {snapshots:?}"))
}

fn select_load_instrument(
    market: &MarketService,
    result: aeris_contracts::ProviderInstrumentSearchResult,
) -> Result<(), String> {
    let candidate = result
        .instruments
        .into_iter()
        .find(|item| {
            item.symbol.starts_with("/ES") && item.instrument_type.as_deref() == Some("Future")
        })
        .ok_or("History load future was not found")?;
    market.select_provider_instrument(
        1,
        SelectProviderInstrument {
            consumer_id: 1,
            selection_generation: 1,
            search_generation: 1,
            provider: "tastytrade".into(),
            symbol: candidate.symbol,
            exchange: candidate.exchange,
            entitlement_id: "tastytrade-authorized".into(),
        },
    )
}

fn select(
    market: &MarketService,
    result: aeris_contracts::ProviderInstrumentSearchResult,
    query: &str,
) -> Result<(), String> {
    println!("Catalog returned {} assets", result.instruments.len());
    let candidate = result
        .instruments
        .into_iter()
        .find(|item| {
            item.symbol == query
                || (matches!(query, "ES" | "/ES")
                    && item.symbol.starts_with("/ES")
                    && item.instrument_type.as_deref() == Some("Future"))
        })
        .ok_or("Requested streamable asset was not found")?;
    println!("Selecting {}", candidate.symbol);
    market.select_provider_instrument(
        1,
        SelectProviderInstrument {
            consumer_id: 1,
            selection_generation: 1,
            search_generation: 1,
            provider: "tastytrade".into(),
            symbol: candidate.symbol,
            exchange: candidate.exchange,
            entitlement_id: "tastytrade-authorized".into(),
        },
    )
}

fn demand(
    market: &MarketService,
    consumer_id: u64,
    instrument: InstallProviderInstrument,
) -> Result<(), String> {
    println!(
        "Resolved {} -> {} ({})",
        instrument.display_symbol, instrument.provider_symbol, instrument.venue_id
    );
    let series = BarSeriesKey {
        provider_id: instrument.provider,
        instrument_id: instrument.instrument_id,
        entitlement_id: instrument.entitlement_id,
        period: BarPeriod::time(60).map_err(|e| e.to_string())?,
        definition_version: 1,
    };
    market.set_demand(
        1,
        consumer_id,
        1,
        &series,
        StreamRequirements::BARS
            .with(MarketStream::Trades)
            .with(MarketStream::Quotes),
    )
}
