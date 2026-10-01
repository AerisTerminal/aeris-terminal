//! Exercises ordinary `MarketService` catalog, selection, history and live demand.
//! Close the desktop first. Credentials remain in the native vault and hosted broker service.
use aeris_contracts::{SearchProviderInstruments, SelectProviderInstrument};
use aeris_market_data::{BarPeriod, BarSeriesKey};
use aeris_market_engine::{MarketStream, StreamRequirements};
use aeris_market_runtime::{MarketRuntimeEvent, MarketService};
use std::{
    thread,
    time::{Duration, Instant},
};
fn run(market: &MarketService) -> Result<(), String> {
    let query = std::env::args().nth(1).unwrap_or_else(|| "/ES".into());
    market.attach(1)?;
    market.register_consumer(1, 1, 1)?;
    market.search_provider_instruments(
        1,
        SearchProviderInstruments {
            consumer_id: 1,
            search_generation: 1,
            provider: "tastytrade".into(),
            query,
            maximum_results: 100,
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut selected = false;
    let mut snapshots = 0;
    let mut tails = 0;
    let mut trades = 0;
    let mut history_check = HistoryCheck::default();
    while Instant::now() < deadline {
        if let Some(event) = market.poll_event(1, 1)? {
            match event {
                MarketRuntimeEvent::ProviderInstrumentSearchResult(result) => {
                    select(market, result)?;
                }
                MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
                    demand(market, selection.instrument)?;
                    selected = true;
                }
                MarketRuntimeEvent::SeriesSnapshot(snapshot) => {
                    snapshots += 1;
                    history_check.accept(market, &snapshot)?;
                }
                MarketRuntimeEvent::SeriesUpdate(_) => {
                    tails += 1;
                }
                MarketRuntimeEvent::TradeTapeSnapshot(tape) => {
                    if trades == 0 || tape.trades.len() >= trades + 1024 {
                        trades = tape.trades.len();
                        let first = tape
                            .trades
                            .iter()
                            .filter_map(|t| t.trade.metadata.timestamps.exchange_unix_nanos)
                            .min();
                        let last = tape
                            .trades
                            .iter()
                            .filter_map(|t| t.trade.metadata.timestamps.exchange_unix_nanos)
                            .max();
                        println!(
                            "Retained ticks={trades}; first exchange nanoseconds={first:?}; last={last:?}"
                        );
                    }
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
        if history_check.switched && tails >= 10 && trades > 0 {
            break;
        }
    }
    println!(
        "Selected={selected}; history snapshots={snapshots}; live updates={tails}; retained ticks={trades}"
    );
    if !selected || snapshots == 0 || tails == 0 || !history_check.switched {
        return Err("Catalog/chart/live integration did not complete".into());
    }
    market.detach(1)?;
    Ok(())
}
#[derive(Default)]
struct HistoryCheck {
    initial_first: Option<i64>,
    provider_generation: Option<u64>,
    backfilled: bool,
    switched: bool,
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
                println!("Five-minute candles loaded on the same session");
            }
            _ => {}
        }
        Ok(())
    }
}
fn main() -> Result<(), String> {
    let market = MarketService::start()?;
    let result = run(&market);
    let shutdown = market.shutdown(Duration::from_secs(10));
    result.and(shutdown)
}

fn select(
    market: &MarketService,
    result: aeris_contracts::ProviderInstrumentSearchResult,
) -> Result<(), String> {
    println!("Catalog returned {} assets", result.instruments.len());
    let query = std::env::args().nth(1).unwrap_or_else(|| "/ES".into());
    let candidate = result
        .instruments
        .into_iter()
        .find(|item| {
            item.symbol == query
                || (query == "/ES"
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
    instrument: aeris_contracts::InstallProviderInstrument,
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
        1,
        1,
        &series,
        StreamRequirements::BARS
            .with(MarketStream::Trades)
            .with(MarketStream::Quotes),
    )
}
