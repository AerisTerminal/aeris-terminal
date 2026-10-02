//! Provider metadata for bounded, read-only futures discovery.

use crate::DATA_SCALE;
use aeris_market_data::parse_decimal_to_fixed;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SearchInstrument {
    pub symbol: String,
    pub instrument_type: String,
    pub exchange: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SearchPage {
    pub data: SearchData,
}
#[derive(Deserialize)]
pub(super) struct SearchData {
    pub items: Vec<SearchInstrument>,
}

#[derive(Clone, Debug)]
pub struct ResolvedInstrument {
    pub symbol: String,
    pub streamer_symbol: String,
    pub venue: String,
    pub instrument_type: String,
    pub tick_size: Option<i64>,
    /// The provider's ordered price bands. A threshold is the exclusive upper
    /// bound for that band; the final band normally has no threshold.
    pub tick_sizes: Vec<PriceIncrementBand>,
    pub point_value: Option<i64>,
    pub currency: Option<String>,
    pub expiration_date: Option<String>,
    pub first_notice_date: Option<String>,
    pub last_trade_date: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceIncrementBand {
    pub value: i64,
    pub threshold: Option<i64>,
}

impl ResolvedInstrument {
    #[must_use]
    pub fn tick_size_for_price(&self, price: Option<i64>) -> Option<i64> {
        let bands = &self.tick_sizes;
        let Some(price) = price else {
            return bands.last().map(|band| band.value).or(self.tick_size);
        };
        bands
            .iter()
            .find(|band| band.threshold.is_some_and(|threshold| price < threshold))
            .or_else(|| bands.last())
            .map(|band| band.value)
            .or(self.tick_size)
    }
}

impl ResolvedInstrument {
    /// Uses authoritative fields already present in the active-futures list.
    /// # Errors
    /// Rejects decimal values that cannot be represented exactly.
    pub fn from_future(future: &FutureInstrument) -> Result<Self, String> {
        let point_value = parse_decimal_to_fixed(&future.notional_multiplier, DATA_SCALE)?;
        let tick_size = parse_decimal_to_fixed(&future.tick_size, DATA_SCALE)?;
        if point_value <= 0 || tick_size <= 0 {
            return Err("Tastytrade futures contract terms invalid".into());
        }
        Ok(Self {
            symbol: future.symbol.clone(),
            streamer_symbol: future.streamer_symbol.clone(),
            venue: future.exchange.clone(),
            instrument_type: "Future".into(),
            tick_size: Some(tick_size),
            tick_sizes: vec![PriceIncrementBand {
                value: tick_size,
                threshold: None,
            }],
            point_value: Some(point_value),
            currency: future.currency.clone(),
            expiration_date: Some(future.expiration_date.clone()),
            first_notice_date: future.first_notice_date.clone(),
            last_trade_date: future.last_trade_date.clone(),
        })
    }
    pub(super) fn from_response(
        response: &Value,
        requested: &SearchInstrument,
    ) -> Result<Self, String> {
        let data = response
            .get("data")
            .ok_or("Tastytrade instrument metadata missing")?;
        let text = |name| {
            data.get(name)
                .and_then(Value::as_str)
                .filter(|s| valid_identity(s))
        };
        let symbol = text("symbol").ok_or("Tastytrade instrument symbol missing")?;
        let streamer_symbol =
            text("streamer-symbol").ok_or("Tastytrade streamer symbol missing")?;
        if symbol != requested.symbol {
            return Err("Tastytrade resolved another instrument".into());
        }
        let venue = text("exchange")
            .or_else(|| text("listed-market"))
            .or(requested.exchange.as_deref())
            .filter(|s| valid_identity(s))
            .ok_or("Tastytrade instrument venue missing")?;
        let tick_sizes = parse_tick_sizes(data)?;
        let point_value = if requested.instrument_type == "Future" {
            Some(parse_decimal_to_fixed(
                text("notional-multiplier").ok_or("Tastytrade futures multiplier missing")?,
                DATA_SCALE,
            )?)
        } else {
            None
        };
        if point_value.is_some_and(|value| value <= 0) {
            return Err("Tastytrade futures multiplier invalid".into());
        }
        let parse_date = |name| -> Result<Option<String>, String> {
            data.get(name)
                .map(|value| {
                    let value = value.as_str().ok_or("Tastytrade futures date invalid")?;
                    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                        .map_err(|_| "Tastytrade futures date invalid")?;
                    Ok(value.to_string())
                })
                .transpose()
        };
        let expiration_date = parse_date("expiration-date")?;
        if requested.instrument_type == "Future" && expiration_date.is_none() {
            return Err("Tastytrade futures expiry missing".into());
        }
        Ok(Self {
            symbol: symbol.into(),
            streamer_symbol: streamer_symbol.into(),
            venue: venue.into(),
            instrument_type: requested.instrument_type.clone(),
            tick_size: tick_sizes.last().map(|band| band.value),
            tick_sizes,
            point_value,
            currency: data
                .get("currency")
                .and_then(Value::as_str)
                .map(str::to_string),
            expiration_date,
            first_notice_date: parse_date("first-notice-date")?,
            last_trade_date: parse_date("last-trade-date")?,
        })
    }
}

fn parse_tick_sizes(data: &Value) -> Result<Vec<PriceIncrementBand>, String> {
    let mut tick_sizes = Vec::new();
    if let Some(tick) = data.get("tick-size") {
        tick_sizes.push(PriceIncrementBand {
            value: parse_decimal_to_fixed(
                tick.as_str().ok_or("Tastytrade tick size invalid")?,
                DATA_SCALE,
            )?,
            threshold: None,
        });
    }
    if let Some(values) = data.get("tick-sizes").and_then(Value::as_array) {
        if values.len() > 64 {
            return Err("Tastytrade tick schedule exceeded bound".into());
        }
        for value in values {
            tick_sizes.push(PriceIncrementBand {
                value: parse_decimal_to_fixed(
                    value
                        .get("value")
                        .and_then(Value::as_str)
                        .ok_or("Tastytrade tick schedule value missing")?,
                    DATA_SCALE,
                )?,
                threshold: value
                    .get("threshold")
                    .map(|threshold| {
                        parse_decimal_to_fixed(
                            threshold
                                .as_str()
                                .ok_or("Tastytrade tick schedule threshold invalid")?,
                            DATA_SCALE,
                        )
                    })
                    .transpose()?,
            });
        }
    }
    if tick_sizes.is_empty()
        || tick_sizes
            .iter()
            .any(|band| band.value <= 0 || band.threshold.is_some_and(|threshold| threshold <= 0))
        || tick_sizes.windows(2).any(|bands| {
            bands[0]
                .threshold
                .zip(bands[1].threshold)
                .is_some_and(|(left, right)| left >= right)
        })
    {
        return Err("Tastytrade tick increment invalid".into());
    }
    Ok(tick_sizes)
}

pub(super) fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 160 && !value.chars().any(char::is_control)
}

/// Provider identities retained without rebuilding futures streamer symbology.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FutureInstrument {
    pub symbol: String,
    pub streamer_symbol: String,
    pub exchange: String,
    pub product_code: String,
    pub expiration_date: String,
    pub active: bool,
    pub active_month: bool,
    pub notional_multiplier: String,
    pub tick_size: String,
    pub first_notice_date: Option<String>,
    pub last_trade_date: Option<String>,
    pub currency: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CatalogPage {
    data: CatalogData,
}

#[derive(Deserialize)]
struct CatalogData {
    items: Vec<FutureInstrument>,
}

impl CatalogPage {
    pub(super) fn validated(self) -> Result<Vec<FutureInstrument>, String> {
        if self.data.items.len() > 100 {
            return Err("Futures catalog exceeded its page bound".to_string());
        }
        for item in &self.data.items {
            if !item.symbol.starts_with('/')
                || !item.streamer_symbol.starts_with('/')
                || !item.streamer_symbol.contains(':')
                || [
                    &item.symbol,
                    &item.streamer_symbol,
                    &item.exchange,
                    &item.product_code,
                ]
                .iter()
                .any(|value| {
                    value.is_empty()
                        || value.len() > 128
                        || value
                            .bytes()
                            .any(|byte| byte.is_ascii_control() || byte == b' ')
                })
                || chrono::NaiveDate::parse_from_str(&item.expiration_date, "%Y-%m-%d").is_err()
                || item.first_notice_date.as_ref().is_some_and(|date| {
                    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err()
                })
                || item.last_trade_date.as_ref().is_some_and(|date| {
                    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err()
                })
                || parse_decimal_to_fixed(&item.notional_multiplier, DATA_SCALE).is_err()
                || parse_decimal_to_fixed(&item.tick_size, DATA_SCALE).is_err()
            {
                return Err("Futures catalog contains invalid required metadata".to_string());
            }
        }
        Ok(self.data.items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_preserves_provider_symbols_and_requires_identity_fields() {
        let fixture = r#"{"data":{"items":[{"symbol":"/ESU3","streamer-symbol":"/ESU23:XCME","exchange":"CME","product-code":"ES","expiration-date":"2023-09-15","notional-multiplier":"50.0","tick-size":"0.25","active":true,"active-month":true}]}}"#;
        let page: CatalogPage = serde_json::from_str(fixture).expect("documented catalog shape");
        assert_eq!(
            page.validated().expect("valid catalog")[0].streamer_symbol,
            "/ESU23:XCME"
        );
        assert!(
            serde_json::from_str::<CatalogPage>(r#"{"data":{"items":[{"symbol":"/ESU3"}]}}"#)
                .is_err()
        );
    }

    #[test]
    fn futures_terms_keep_exact_point_value_and_reject_excess_precision() {
        let future: FutureInstrument = serde_json::from_str(
            r#"{
            "symbol":"/ESZ6","streamer-symbol":"/ESZ26:XCME","exchange":"CME",
            "product-code":"ES","expiration-date":"2026-12-18","active":true,
            "active-month":true,"notional-multiplier":"50.25","tick-size":"0.25",
            "first-notice-date":"2026-12-17","last-trade-date":"2026-12-18"
        }"#,
        )
        .expect("documented futures fields");
        let resolved = ResolvedInstrument::from_future(&future).expect("exact terms");
        assert_eq!(resolved.point_value, Some(5_025_000_000));
        assert_eq!(resolved.tick_size, Some(25_000_000));
        assert_eq!(resolved.expiration_date.as_deref(), Some("2026-12-18"));
        assert_eq!(resolved.currency, None);
        let mut invalid = future;
        invalid.notional_multiplier = "50.000000001".into();
        assert!(ResolvedInstrument::from_future(&invalid).is_err());
    }

    #[test]
    fn equity_tick_bands_select_the_current_price_band() {
        let requested = SearchInstrument {
            symbol: "AAPL".into(),
            instrument_type: "Equity".into(),
            exchange: Some("NASDAQ".into()),
            description: None,
        };
        let response = serde_json::json!({
            "data": {
                "symbol": "AAPL",
                "streamer-symbol": "AAPL",
                "exchange": "NASDAQ",
                "tick-sizes": [
                    {"value": "0.0001", "threshold": "1.0"},
                    {"value": "0.01"}
                ]
            }
        });
        let resolved = ResolvedInstrument::from_response(&response, &requested)
            .expect("banded equity metadata");
        assert_eq!(
            resolved.tick_size_for_price(Some(331 * 100_000_000)),
            Some(1_000_000)
        );
        assert_eq!(resolved.tick_size_for_price(Some(50_000_000)), Some(10_000));
        assert_eq!(resolved.tick_size_for_price(None), Some(1_000_000));
    }
}
