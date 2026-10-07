//! Hyperliquid listed-market screen, served by the catalog worker.
//!
//! Statistics are fetched with two public info requests and retained for a short bounded age,
//! so any number of screening consumers cost at most one refresh per age window. Rows join the
//! retained catalog (selection identity) with the statistics (keyed by exact wire coin);
//! markets without statistics, such as builder perps, are not listed.

use std::{
    cmp::Reverse,
    collections::BTreeMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use aeris_contracts::{ProviderInstrumentSummary, ProviderMarketStatistics};
use aeris_hyperliquid_market_adapter::{
    FUNDING_INTERVAL_SECONDS, FUNDING_RATE_SCALE, HyperliquidCatalog, HyperliquidHttpConfig,
    HyperliquidInstrument, HyperliquidMarketKind, HyperliquidMarketStatistics,
    NORMALIZED_PRICE_SCALE, STATISTICS_NOTIONAL_SCALE, fetch_market_statistics,
};

/// Oldest retained statistics a screen request is answered with before refetching.
const STATISTICS_MAXIMUM_AGE: Duration = Duration::from_secs(3);

/// Last successfully fetched statistics.
#[derive(Default)]
pub(crate) struct RetainedMarketStatistics {
    fetched: Option<FetchedStatistics>,
}

struct FetchedStatistics {
    at: Instant,
    captured_at_unix_millis: i64,
    markets: BTreeMap<String, HyperliquidMarketStatistics>,
}

impl RetainedMarketStatistics {
    /// Refetches when the retained statistics are missing or older than the bounded age.
    /// A failed refresh keeps serving the previous statistics, whose capture time tells the
    /// consumer how old they are; it only fails when nothing was ever fetched.
    pub(crate) fn refresh(
        &mut self,
        config: HyperliquidHttpConfig,
        now: Instant,
    ) -> Result<(), String> {
        if self.fetched.as_ref().is_some_and(|fetched| {
            now.saturating_duration_since(fetched.at) < STATISTICS_MAXIMUM_AGE
        }) {
            return Ok(());
        }
        match fetch_market_statistics(config) {
            Ok(markets) => {
                self.fetched = Some(FetchedStatistics {
                    at: now,
                    captured_at_unix_millis: unix_millis_now(),
                    markets,
                });
                Ok(())
            }
            Err(_) if self.fetched.is_some() => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Capture time and screen rows, or `None` before the first successful fetch.
    pub(crate) fn screen(
        &self,
        catalog: &HyperliquidCatalog,
        maximum_results: usize,
    ) -> Option<(i64, Vec<ProviderMarketStatistics>)> {
        let fetched = self.fetched.as_ref()?;
        Some((
            fetched.captured_at_unix_millis,
            screen_rows(catalog, &fetched.markets, maximum_results),
        ))
    }
}

/// Joins catalog identity with statistics, most traded first, bounded to `maximum_results`.
pub(crate) fn screen_rows(
    catalog: &HyperliquidCatalog,
    statistics: &BTreeMap<String, HyperliquidMarketStatistics>,
    maximum_results: usize,
) -> Vec<ProviderMarketStatistics> {
    let mut rows = catalog
        .instruments
        .values()
        .filter_map(|instrument| {
            statistics
                .get(&instrument.wire_coin)
                .map(|statistics| (instrument, *statistics))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|(left, left_statistics), (right, right_statistics)| {
        Reverse(left_statistics.day_notional_volume)
            .cmp(&Reverse(right_statistics.day_notional_volume))
            .then_with(|| left.display.cmp(&right.display))
    });
    rows.into_iter()
        .take(maximum_results)
        .map(|(instrument, statistics)| ProviderMarketStatistics {
            instrument: instrument_summary(instrument),
            price_scale: NORMALIZED_PRICE_SCALE,
            mark_price: statistics.mark_price,
            previous_day_price: statistics.previous_day_price,
            notional_scale: STATISTICS_NOTIONAL_SCALE,
            day_notional_volume: statistics.day_notional_volume,
            open_interest_notional: statistics.open_interest_notional,
            funding_rate_scale: FUNDING_RATE_SCALE,
            funding_rate: statistics.funding_rate,
            funding_interval_seconds: statistics.funding_rate.map(|_| FUNDING_INTERVAL_SECONDS),
        })
        .collect()
}

/// Provider-neutral search/screen presentation for one catalog instrument.
pub(crate) fn instrument_summary(instrument: &HyperliquidInstrument) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        symbol: instrument.wire_coin.clone(),
        display_symbol: instrument.display.clone(),
        exchange: instrument.venue.clone(),
        name: None,
        product_code: None,
        instrument_type: Some(match &instrument.kind {
            HyperliquidMarketKind::CorePerp => "perpetual".to_string(),
            HyperliquidMarketKind::Spot { .. } => "spot".to_string(),
            HyperliquidMarketKind::BuilderPerp { dex } => format!("builder-perpetual:{dex}"),
        }),
        expiration_date: None,
    }
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
    use serde_json::json;

    fn catalog() -> HyperliquidCatalog {
        aeris_hyperliquid_market_adapter::decode_catalog(
            &aeris_hyperliquid_market_adapter::RawMetaBundle {
                core_perp_meta: json!({"universe": [
                    {"name": "BTC", "szDecimals": 5},
                    {"name": "ETH", "szDecimals": 4},
                    {"name": "DOGE", "szDecimals": 0},
                ]}),
                spot_meta: json!({
                    "universe": [],
                    "tokens": [{"name": "USDC", "szDecimals": 8, "index": 0}],
                }),
                perp_dexs: json!([null]),
                builder_metas: Vec::new(),
            },
        )
        .expect("catalog")
    }

    fn statistics(volume: Option<i64>) -> HyperliquidMarketStatistics {
        HyperliquidMarketStatistics {
            mark_price: Some(100),
            previous_day_price: Some(90),
            day_notional_volume: volume,
            open_interest_notional: Some(5),
            funding_rate: Some(125_000),
        }
    }

    #[test]
    fn rows_rank_by_volume_and_skip_markets_without_statistics() {
        let markets = BTreeMap::from([
            ("BTC".to_string(), statistics(Some(10))),
            ("ETH".to_string(), statistics(Some(30))),
            ("UNLISTED".to_string(), statistics(Some(99))),
        ]);
        let rows = screen_rows(&catalog(), &markets, 16);
        assert_eq!(
            rows.iter()
                .map(|row| row.instrument.symbol.as_str())
                .collect::<Vec<_>>(),
            ["ETH", "BTC"],
            "DOGE has no statistics and UNLISTED is not in the catalog"
        );
        let first = &rows[0];
        assert_eq!(first.price_scale, NORMALIZED_PRICE_SCALE);
        assert_eq!(first.notional_scale, STATISTICS_NOTIONAL_SCALE);
        assert_eq!(
            first.funding_interval_seconds,
            Some(FUNDING_INTERVAL_SECONDS)
        );
        assert_eq!(
            first.instrument.instrument_type.as_deref(),
            Some("perpetual")
        );
    }

    #[test]
    fn unreported_volume_sorts_last_and_results_stay_bounded() {
        let markets = BTreeMap::from([
            ("BTC".to_string(), statistics(None)),
            ("ETH".to_string(), statistics(Some(1))),
            ("DOGE".to_string(), statistics(Some(2))),
        ]);
        let rows = screen_rows(&catalog(), &markets, 2);
        assert_eq!(
            rows.iter()
                .map(|row| row.instrument.symbol.as_str())
                .collect::<Vec<_>>(),
            ["DOGE", "ETH"]
        );
        let mut funding_free = statistics(Some(3));
        funding_free.funding_rate = None;
        let rows = screen_rows(
            &catalog(),
            &BTreeMap::from([("BTC".to_string(), funding_free)]),
            4,
        );
        assert_eq!(rows[0].funding_interval_seconds, None);
    }
}
