//! Public no-chart quote lease. Run with `-- BTC 30` for a 30-second sample.

use aeris_instruments::InstrumentId;
use aeris_market_engine::{InstrumentStreamKey, LeaseOwner};
use aeris_market_runtime::{MarketRuntimeEvent, MarketService};
use aeris_market_runtime::{SearchProviderInstruments, SelectProviderInstrument};
use std::{
    thread,
    time::{Duration, Instant},
};

fn run(market: &MarketService, symbol: &str, seconds: u64) -> Result<(), String> {
    market.attach(1)?;
    market.register_consumer(1, 1, 1)?;
    market.search_provider_instruments(
        1,
        SearchProviderInstruments {
            consumer_id: 1,
            search_generation: 1,
            startup_resolve: false,
            provider: "hyperliquid".into(),
            query: symbol.into(),
            maximum_results: 10,
            categories: aeris_market_runtime::InstrumentSearchCategories::ALL,
            exchange: None,
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut selected = false;
    let mut lease = None;
    let mut seen = None;
    let mut count = 0;
    while Instant::now() < deadline {
        if let Some(event) = market.poll_event(1, 1)? {
            match event.value {
                MarketRuntimeEvent::ProviderInstrumentSearchResult(result) if !selected => {
                    if let Some(item) = result.instruments.into_iter().find(|item| {
                        item.symbol == symbol || item.symbol.starts_with(&format!("{symbol}-"))
                    }) {
                        selected = true;
                        market.select_provider_instrument(
                            1,
                            SelectProviderInstrument {
                                consumer_id: 1,
                                selection_generation: 1,
                                search_generation: 1,
                                startup_resolve: false,
                                provider: "hyperliquid".into(),
                                symbol: item.symbol,
                                exchange: item.exchange,
                                entitlement_id: "hyperliquid-public".into(),
                            },
                        )?;
                    }
                }
                MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
                    let instrument = selection.instrument;
                    println!("NO_CHART_DEMAND instrument={}", instrument.instrument_id);
                    lease = Some(
                        market.subscribe_quotes(
                            LeaseOwner::Trading,
                            InstrumentStreamKey {
                                provider: instrument.provider,
                                instrument: InstrumentId::try_new(instrument.instrument_id)
                                    .map_err(|error| error.to_string())?,
                                entitlement: Some(instrument.entitlement_id),
                            },
                        )?,
                    );
                }
                MarketRuntimeEvent::ProviderCatalogRejected { rejection, .. } => {
                    return Err(format!("catalog rejected: {:?}", rejection.reason));
                }
                _ => {}
            }
        }
        if let Some(state) = lease
            .as_ref()
            .and_then(aeris_market_runtime::QuoteSubscription::latest)
        {
            let quote = state.quote;
            if quote.bid.units() <= 0
                || quote.ask.units() <= quote.bid.units()
                || quote.bid.scale() != quote.ask.scale()
            {
                return Err("invalid live best bid or offer".into());
            }
            let touch = (quote.bid.units(), quote.ask.units());
            if seen != Some(touch) {
                println!(
                    "BBO bid={} ask={} scale={} source_sequence={:?}",
                    touch.0,
                    touch.1,
                    quote.bid.scale(),
                    quote.source_sequence
                );
                seen = Some(touch);
                count += 1;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    drop(lease);
    market.detach(1)?;
    if count < 5 {
        return Err(format!("expected five live BBO updates, received {count}"));
    }
    println!("BBO updates={count} chart_series=0");
    Ok(())
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let symbol = args.next().unwrap_or_else(|| "BTC".into());
    let seconds = args.next().and_then(|arg| arg.parse().ok()).unwrap_or(30);
    // A public-feed probe must never read production vault entries or
    // overlap the desktop's data root.
    let root = aeris_platform_runtime::data_root::initialize_data_root(
        aeris_platform_runtime::data_root::DataRootMode::Validation {
            root: std::env::temp_dir()
                .join("aeris_validation")
                .join(format!("hyperliquid_bbo-{}", std::process::id())),
        },
    )
    .map_err(|error| error.to_string())?;
    let _lock = aeris_platform_runtime::data_root::InstanceLock::acquire(root)
        .map_err(|error| error.to_string())?;
    let market = MarketService::start()?;
    let result = run(&market, &symbol, seconds);
    let shutdown = market.shutdown(Duration::from_secs(10));
    result?;
    shutdown
}
