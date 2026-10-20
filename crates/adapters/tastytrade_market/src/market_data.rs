//! Bounded REST quote snapshots from `/market-data/by-type`.

use crate::{DATA_SCALE, catalog::valid_identity};
use aeris_market_data::parse_decimal_to_fixed;
use serde::Deserialize;

/// The provider caps one request at 100 symbols across every instrument-type parameter.
pub const MARKET_DATA_BATCH_LIMIT: usize = 100;

/// Instrument types the snapshot endpoint is queried with.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MarketDataType {
    Equity,
    Index,
    Future,
}

impl MarketDataType {
    const ALL: [Self; 3] = [Self::Equity, Self::Index, Self::Future];

    /// The query-string parameter that carries this type's symbols.
    const fn parameter(self) -> &'static str {
        match self {
            Self::Equity => "equity",
            Self::Index => "index",
            Self::Future => "future",
        }
    }

    /// The `instrument-type` the provider reports for this type.
    #[must_use]
    pub const fn instrument_type(self) -> &'static str {
        match self {
            Self::Equity => "Equity",
            Self::Index => "Index",
            Self::Future => "Future",
        }
    }
}

/// One instrument's provider quote snapshot. Prices and volume use `DATA_SCALE`; an absent
/// value was not reported by the provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketDataSnapshot {
    pub kind: MarketDataType,
    pub symbol: String,
    pub mark: Option<i64>,
    pub last: Option<i64>,
    pub previous_close: Option<i64>,
    /// Shares or contracts traded in the current session.
    pub volume: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct MarketDataPage {
    data: MarketDataItems,
}

#[derive(Deserialize)]
struct MarketDataItems {
    items: Vec<MarketDataItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct MarketDataItem {
    symbol: String,
    instrument_type: String,
    mark: Option<String>,
    last: Option<String>,
    prev_close: Option<String>,
    volume: Option<String>,
}

/// The query parameters for one batch: each type's symbols, comma-delimited.
pub(super) fn batch_query(
    batch: &[(MarketDataType, String)],
) -> Result<Vec<(&'static str, String)>, String> {
    if batch.is_empty() || batch.len() > MARKET_DATA_BATCH_LIMIT {
        return Err("Tastytrade market-data batch size is invalid".into());
    }
    if batch
        .iter()
        .any(|(_, symbol)| !valid_identity(symbol) || symbol.contains(','))
    {
        return Err("Tastytrade market-data symbol is invalid".into());
    }
    Ok(MarketDataType::ALL
        .into_iter()
        .filter_map(|kind| {
            let symbols = batch
                .iter()
                .filter(|(requested, _)| *requested == kind)
                .map(|(_, symbol)| symbol.as_str())
                .collect::<Vec<_>>();
            (!symbols.is_empty()).then(|| (kind.parameter(), symbols.join(",")))
        })
        .collect())
}

impl MarketDataPage {
    /// Keeps one snapshot per requested instrument. The provider may omit a symbol it does
    /// not quote, but an unrequested, duplicated or malformed item rejects the page.
    pub(super) fn validated(
        self,
        batch: &[(MarketDataType, String)],
    ) -> Result<Vec<MarketDataSnapshot>, String> {
        if self.data.items.len() > batch.len() {
            return Err("Tastytrade market data exceeded its request".into());
        }
        let mut snapshots: Vec<MarketDataSnapshot> = Vec::with_capacity(self.data.items.len());
        for item in self.data.items {
            let kind = batch
                .iter()
                .find(|(kind, symbol)| {
                    *symbol == item.symbol && kind.instrument_type() == item.instrument_type
                })
                .map(|(kind, _)| *kind)
                .ok_or("Tastytrade market data returned an unrequested instrument")?;
            if snapshots
                .iter()
                .any(|snapshot| snapshot.kind == kind && snapshot.symbol == item.symbol)
            {
                return Err("Tastytrade market data repeated an instrument".into());
            }
            let volume = decimal(item.volume.as_deref())?;
            if volume.is_some_and(|volume| volume < 0) {
                return Err("Tastytrade market-data volume is negative".into());
            }
            snapshots.push(MarketDataSnapshot {
                kind,
                symbol: item.symbol,
                mark: decimal(item.mark.as_deref())?,
                last: decimal(item.last.as_deref())?,
                previous_close: decimal(item.prev_close.as_deref())?,
                volume,
            });
        }
        Ok(snapshots)
    }
}

fn decimal(value: Option<&str>) -> Result<Option<i64>, String> {
    value
        .map(|value| {
            parse_decimal_to_fixed(value, DATA_SCALE)
                .map_err(|_| "Tastytrade market-data value is not an exact decimal".to_string())
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch() -> Vec<(MarketDataType, String)> {
        vec![
            (MarketDataType::Equity, "AAPL".into()),
            (MarketDataType::Equity, "SPY".into()),
            (MarketDataType::Index, "SPX".into()),
            (MarketDataType::Future, "/ESM6".into()),
        ]
    }

    fn page(items: &serde_json::Value) -> MarketDataPage {
        serde_json::from_value(serde_json::json!({"data": {"items": items}, "pagination": null}))
            .expect("documented market-data page")
    }

    #[test]
    fn query_groups_symbols_by_type_within_the_batch_limit() {
        assert_eq!(
            batch_query(&batch()).expect("valid batch"),
            [
                ("equity", "AAPL,SPY".to_string()),
                ("index", "SPX".to_string()),
                ("future", "/ESM6".to_string()),
            ]
        );
        let oversized = (0..=MARKET_DATA_BATCH_LIMIT)
            .map(|index| (MarketDataType::Equity, format!("S{index}")))
            .collect::<Vec<_>>();
        assert!(batch_query(&oversized).is_err());
        assert!(batch_query(&[(MarketDataType::Equity, "A,B".into())]).is_err());
    }

    #[test]
    fn documented_items_keep_exact_values_and_omitted_symbols() {
        let snapshots = page(&serde_json::json!([
            {
                "symbol": "AAPL",
                "instrument-type": "Equity",
                "updated-at": "2025-04-29T21:33:25.535Z",
                "bid": "210.55",
                "mark": "210.55",
                "last": "210.511",
                "prev-close": "210.14",
                "volume": "35348839.0"
            },
            {"symbol": "SPX", "instrument-type": "Index", "last": "5560.83", "prev-close": "5528.75"}
        ]))
        .validated(&batch())
        .expect("valid page");
        assert_eq!(
            snapshots,
            [
                MarketDataSnapshot {
                    kind: MarketDataType::Equity,
                    symbol: "AAPL".into(),
                    mark: Some(21_055_000_000),
                    last: Some(21_051_100_000),
                    previous_close: Some(21_014_000_000),
                    volume: Some(3_534_883_900_000_000),
                },
                MarketDataSnapshot {
                    kind: MarketDataType::Index,
                    symbol: "SPX".into(),
                    mark: None,
                    last: Some(556_083_000_000),
                    previous_close: Some(552_875_000_000),
                    volume: None,
                },
            ]
        );
    }

    #[test]
    fn unrequested_repeated_or_inexact_items_reject_the_page() {
        let unrequested = page(&serde_json::json!([
            {"symbol": "SPX", "instrument-type": "Equity", "last": "1"}
        ]));
        assert!(unrequested.validated(&batch()).is_err());
        let repeated = page(&serde_json::json!([
            {"symbol": "SPY", "instrument-type": "Equity"},
            {"symbol": "SPY", "instrument-type": "Equity"}
        ]));
        assert!(repeated.validated(&batch()).is_err());
        let inexact = page(&serde_json::json!([
            {"symbol": "SPY", "instrument-type": "Equity", "mark": "1.123456789"}
        ]));
        assert!(inexact.validated(&batch()).is_err());
        let negative = page(&serde_json::json!([
            {"symbol": "SPY", "instrument-type": "Equity", "volume": "-1"}
        ]));
        assert!(negative.validated(&batch()).is_err());
    }
}
