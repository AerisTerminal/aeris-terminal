//! tastytrade market screen over a fixed, curated universe, served by the catalog worker.
//!
//! The provider has no listing endpoint, so the screen quotes major indices, broad ETFs,
//! large-cap equities and the front month of liquid futures products through bounded
//! `/market-data/by-type` snapshots. Snapshots are retained for a short age so any number of
//! screening consumers cost at most one fetch per age window.

use super::{
    AtomicBool, AtomicU64, BrokerApi, Duration, FutureInstrument, Instant, Ordering,
    ProviderCatalogRejected, ProviderCatalogRejectionReason, SearchInstrument, instrument_summary,
};
use aeris_contracts::{
    ProviderMarketKind, ProviderMarketScreen, ProviderMarketStatistics, ScreenProviderMarkets,
};
use aeris_observability::diagnostic;
use aeris_tastytrade_market_adapter::{DATA_SCALE, MarketDataSnapshot, MarketDataType};
use std::cmp::Reverse;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const INDICES: &[&str] = &["SPX", "NDX", "RUT", "VIX"];

const EXCHANGE_TRADED_FUNDS: &[&str] = &[
    "SPY", "QQQ", "IWM", "DIA", "VOO", "VTI", "TLT", "IEF", "HYG", "LQD", "GLD", "SLV", "USO",
    "UNG", "XLE", "XLF", "XLK", "XLV", "XLY", "XLP", "XLI", "XLU", "XLB", "XLRE", "XLC", "SMH",
    "ARKK", "EEM", "EFA", "FXI", "KWEB", "TQQQ", "SQQQ", "SOXL", "UVXY", "IBIT",
];

const LARGE_CAP_EQUITIES: &[&str] = &[
    "AAPL", "MSFT", "NVDA", "AMZN", "GOOGL", "META", "BRK/B", "AVGO", "TSLA", "LLY", "JPM", "V",
    "WMT", "XOM", "UNH", "MA", "ORCL", "COST", "HD", "PG", "JNJ", "NFLX", "ABBV", "BAC", "CRM",
    "KO", "CVX", "MRK", "AMD", "PEP", "TMO", "ADBE", "WFC", "CSCO", "ACN", "LIN", "MCD", "ABT",
    "IBM", "PM", "GE", "DIS", "INTU", "CAT", "TXN", "QCOM", "VZ", "AMGN", "NOW", "ISRG", "GS",
    "DHR", "PFE", "CMCSA", "UBER", "AMAT", "NEE", "RTX", "T", "SPGI", "LOW", "UNP", "PGR", "HON",
    "BKNG", "BLK", "COP", "MS", "C", "AXP", "SYK", "LMT", "PANW", "MU", "SCHW", "ADI", "PLTR",
    "BA", "INTC", "KLAC", "LRCX", "DE", "SBUX", "MDT", "GILD", "BMY", "CB", "NKE", "PYPL", "SHOP",
    "COIN", "MSTR", "SNOW", "ABNB", "F", "GM", "SMCI", "ARM", "CRWD", "ANET",
];

/// Futures products screened by their front month: equity index, energy, metals, rates,
/// currencies, grains and volatility.
const FUTURES_PRODUCTS: &[&str] = &[
    "ES", "NQ", "RTY", "YM", "MES", "MNQ", "CL", "NG", "GC", "SI", "HG", "ZB", "ZN", "ZF", "ZT",
    "6E", "6J", "6B", "6A", "ZC", "ZS", "ZW", "VX",
];

/// Oldest retained snapshot a screen request is answered with before refetching. The
/// provider recommends streaming over frequent polling, so this stays above the screener's
/// refresh cadence.
const SNAPSHOT_MAXIMUM_AGE: Duration = Duration::from_secs(10);

/// Last successfully fetched screen.
#[derive(Default)]
pub(super) struct RetainedScreen {
    fetched: Option<FetchedScreen>,
}

struct FetchedScreen {
    at: Instant,
    captured_at_unix_millis: i64,
    markets: Vec<ProviderMarketStatistics>,
}

impl RetainedScreen {
    /// Drops statistics fetched under a retired authorization.
    pub(super) fn clear(&mut self) {
        self.fetched = None;
    }

    /// Answers one screen, refetching when the retained snapshot is missing or older than
    /// the bounded age. A failed refresh keeps serving the previous snapshot, whose capture
    /// time tells the consumer how old it is; it rejects only when nothing was ever fetched.
    pub(super) fn answer(
        &mut self,
        screen: &ScreenProviderMarkets,
        futures: &mut Vec<FutureInstrument>,
        api: &BrokerApi,
        stop: &Arc<AtomicBool>,
        generation: &AtomicU64,
    ) -> super::CatalogEvent {
        let now = Instant::now();
        if self
            .fetched
            .as_ref()
            .is_none_or(|fetched| now.saturating_duration_since(fetched.at) >= SNAPSHOT_MAXIMUM_AGE)
        {
            match fetch(futures, api, stop) {
                Ok(markets) => {
                    self.fetched = Some(FetchedScreen {
                        at: now,
                        captured_at_unix_millis: unix_millis_now(),
                        markets,
                    });
                }
                Err(error) => diagnostic!("Aeris tastytrade market screen is unavailable: {error}"),
            }
        }
        let maximum_results = usize::try_from(screen.maximum_results).unwrap_or(usize::MAX);
        match &self.fetched {
            Some(fetched) => super::CatalogEvent::ScreenCompleted(ProviderMarketScreen {
                consumer_id: screen.consumer_id,
                provider: "tastytrade".into(),
                provider_generation: generation.load(Ordering::Acquire),
                screen_generation: screen.screen_generation,
                captured_at_unix_millis: fetched.captured_at_unix_millis,
                markets: fetched
                    .markets
                    .iter()
                    .take(maximum_results)
                    .cloned()
                    .collect(),
            }),
            None => super::CatalogEvent::ScreenRejected(ProviderCatalogRejected {
                consumer_id: screen.consumer_id,
                provider: "tastytrade".into(),
                provider_generation: Some(generation.load(Ordering::Acquire)),
                command_generation: screen.screen_generation,
                reason: ProviderCatalogRejectionReason::DispatchUnavailable,
            }),
        }
    }
}

fn fetch(
    futures: &mut Vec<FutureInstrument>,
    api: &BrokerApi,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<ProviderMarketStatistics>, String> {
    if futures.is_empty() {
        match api.futures(stop) {
            Ok(catalog) => *futures = catalog,
            // Equities and indices remain screenable without the futures catalog.
            Err(error) => diagnostic!("Aeris tastytrade screen futures are unavailable: {error}"),
        }
    }
    let listed = universe(futures);
    let requests = listed
        .iter()
        .map(|(kind, item)| (*kind, item.symbol.clone()))
        .collect::<Vec<_>>();
    let snapshots = api.call(stop, |client, capability| {
        client.market_data(capability, &requests, stop)
    })?;
    Ok(screen_rows(&listed, snapshots))
}

/// Every screened instrument with the search identity its selection resolves through.
pub(super) fn universe(futures: &[FutureInstrument]) -> Vec<(MarketDataType, SearchInstrument)> {
    let listed = |kind: MarketDataType, symbol: &str| {
        (
            kind,
            SearchInstrument {
                symbol: symbol.to_string(),
                instrument_type: kind.instrument_type().to_string(),
                exchange: None,
                description: None,
            },
        )
    };
    let mut instruments = INDICES
        .iter()
        .map(|symbol| listed(MarketDataType::Index, symbol))
        .chain(
            EXCHANGE_TRADED_FUNDS
                .iter()
                .chain(LARGE_CAP_EQUITIES)
                .map(|symbol| listed(MarketDataType::Equity, symbol)),
        )
        .collect::<Vec<_>>();
    instruments.extend(FUTURES_PRODUCTS.iter().filter_map(|product| {
        let front = futures
            .iter()
            .filter(|future| future.active && future.product_code == *product)
            .min_by(|left, right| {
                (!left.active_month, &left.expiration_date)
                    .cmp(&(!right.active_month, &right.expiration_date))
            })?;
        Some((
            MarketDataType::Future,
            SearchInstrument {
                symbol: front.symbol.clone(),
                instrument_type: MarketDataType::Future.instrument_type().to_string(),
                exchange: Some(front.exchange.clone()),
                description: Some(front.product_code.clone()),
            },
        ))
    }));
    instruments
}

/// The screened instrument a selection names, by its exact symbol and catalog venue.
pub(super) fn listed_instrument(
    symbol: &str,
    exchange: &str,
    futures: &[FutureInstrument],
) -> Option<SearchInstrument> {
    universe(futures)
        .into_iter()
        .map(|(_, item)| item)
        .find(|item| item.symbol == symbol && super::catalog_venue(item) == exchange)
}

/// Joins snapshots with their listed identity, most traded first; instruments the provider
/// did not quote are not listed.
fn screen_rows(
    listed: &[(MarketDataType, SearchInstrument)],
    snapshots: Vec<MarketDataSnapshot>,
) -> Vec<ProviderMarketStatistics> {
    let mut rows = snapshots
        .into_iter()
        .filter_map(|snapshot| {
            let (_, item) = listed
                .iter()
                .find(|(kind, item)| *kind == snapshot.kind && item.symbol == snapshot.symbol)?;
            Some(ProviderMarketStatistics {
                instrument: instrument_summary(item),
                kind: Some(match snapshot.kind {
                    MarketDataType::Equity => ProviderMarketKind::Equity,
                    MarketDataType::Index => ProviderMarketKind::Index,
                    MarketDataType::Future => ProviderMarketKind::Future,
                }),
                price_scale: DATA_SCALE,
                mark_price: snapshot.mark.or(snapshot.last),
                previous_day_price: snapshot.previous_close,
                volume_scale: DATA_SCALE,
                day_volume: snapshot.volume,
                ..ProviderMarketStatistics::default()
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        Reverse(left.day_volume)
            .cmp(&Reverse(right.day_volume))
            .then_with(|| {
                left.instrument
                    .display_symbol
                    .cmp(&right.instrument.display_symbol)
            })
    });
    rows
}

fn unix_millis_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn future(
        symbol: &str,
        product: &str,
        expiration: &str,
        active_month: bool,
    ) -> FutureInstrument {
        FutureInstrument {
            symbol: symbol.into(),
            streamer_symbol: format!("{symbol}:XCME"),
            exchange: "CME".into(),
            product_code: product.into(),
            expiration_date: expiration.into(),
            active: true,
            active_month,
            notional_multiplier: "50".into(),
            tick_size: "0.25".into(),
            first_notice_date: None,
            last_trade_date: None,
            currency: Some("USD".into()),
        }
    }

    #[test]
    fn universe_is_bounded_unique_and_screens_each_futures_front_month() {
        let futures = [
            future("/ESZ6", "ES", "2026-12-18", false),
            future("/ESU6", "ES", "2026-09-18", true),
            future("/NQU6", "NQ", "2026-09-18", false),
            future("/NQZ6", "NQ", "2026-12-18", false),
        ];
        let listed = universe(&futures);
        let futures_listed = listed
            .iter()
            .filter(|(kind, _)| *kind == MarketDataType::Future)
            .map(|(_, item)| item.symbol.as_str())
            .collect::<Vec<_>>();
        assert_eq!(futures_listed, ["/ESU6", "/NQU6"]);
        let mut identities = listed
            .iter()
            .map(|(kind, item)| (*kind, item.symbol.as_str()))
            .collect::<Vec<_>>();
        let total = identities.len();
        identities.sort_unstable();
        identities.dedup();
        assert_eq!(
            identities.len(),
            total,
            "every screened instrument is unique"
        );
        assert!(
            total <= 3 * aeris_tastytrade_market_adapter::MARKET_DATA_BATCH_LIMIT,
            "a screen costs at most three provider requests"
        );
        let es = listed_instrument("/ESU6", "CME", &futures).expect("listed future");
        assert_eq!(es.description.as_deref(), Some("ES"));
        let spy = listed_instrument("SPY", "Equity", &futures).expect("listed fund");
        assert_eq!(spy.instrument_type, "Equity");
        assert!(listed_instrument("SPY", "Index", &futures).is_none());
    }

    #[test]
    fn rows_join_listed_identity_rank_by_volume_and_fall_back_to_last_price() {
        let futures = [future("/ESU6", "ES", "2026-09-18", true)];
        let listed = universe(&futures);
        let snapshot = |kind, symbol: &str, mark, last, volume| MarketDataSnapshot {
            kind,
            symbol: symbol.into(),
            mark,
            last,
            previous_close: Some(90),
            volume,
        };
        let rows = screen_rows(
            &listed,
            vec![
                snapshot(MarketDataType::Index, "SPX", None, Some(100), None),
                snapshot(MarketDataType::Equity, "SPY", Some(101), Some(102), Some(5)),
                snapshot(MarketDataType::Future, "/ESU6", Some(103), None, Some(9)),
            ],
        );
        let summary = rows
            .iter()
            .map(|row| {
                (
                    row.instrument.display_symbol.as_str(),
                    row.kind,
                    row.mark_price,
                    row.day_volume,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            [
                ("ESU6", Some(ProviderMarketKind::Future), Some(103), Some(9)),
                ("SPY", Some(ProviderMarketKind::Equity), Some(101), Some(5)),
                ("SPX", Some(ProviderMarketKind::Index), Some(100), None),
            ]
        );
        assert!(rows.iter().all(|row| row.price_scale == DATA_SCALE
            && row.day_notional_volume.is_none()
            && row.funding_rate.is_none()));
        assert_eq!(rows[0].instrument.exchange, "CME");
        assert_eq!(rows[2].instrument.exchange, "Index");
    }
}
