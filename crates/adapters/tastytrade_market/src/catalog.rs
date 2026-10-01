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
}

impl ResolvedInstrument {
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
        let mut ticks = Vec::new();
        if let Some(tick) = data.get("tick-size") {
            ticks.push(parse_decimal_to_fixed(
                tick.as_str().ok_or("Tastytrade tick size invalid")?,
                DATA_SCALE,
            )?);
        }
        if let Some(values) = data.get("tick-sizes").and_then(Value::as_array) {
            if values.len() > 64 {
                return Err("Tastytrade tick schedule exceeded bound".into());
            }
            for value in values {
                ticks.push(parse_decimal_to_fixed(
                    value
                        .get("value")
                        .and_then(Value::as_str)
                        .ok_or("Tastytrade tick schedule value missing")?,
                    DATA_SCALE,
                )?);
            }
        }
        if ticks.iter().any(|tick| *tick <= 0) {
            return Err("Tastytrade tick increment invalid".into());
        }
        Ok(Self {
            symbol: symbol.into(),
            streamer_symbol: streamer_symbol.into(),
            venue: venue.into(),
            instrument_type: requested.instrument_type.clone(),
            tick_size: ticks.into_iter().min(),
        })
    }
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
    pub(super) fn validated(self, product: &str) -> Result<Vec<FutureInstrument>, String> {
        if self.data.items.len() > 100 {
            return Err("Futures catalog exceeded its page bound".to_string());
        }
        for item in &self.data.items {
            if item.product_code != product
                || !item.symbol.starts_with('/')
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
        let fixture = r#"{"data":{"items":[{"symbol":"/ESU3","streamer-symbol":"/ESU23:XCME","exchange":"CME","product-code":"ES","expiration-date":"2023-09-15","active":true,"active-month":true}]}}"#;
        let page: CatalogPage = serde_json::from_str(fixture).expect("documented catalog shape");
        assert_eq!(
            page.validated("ES").expect("valid catalog")[0].streamer_symbol,
            "/ESU23:XCME"
        );
        let page: CatalogPage = serde_json::from_str(fixture).expect("documented shape");
        assert!(page.validated("NQ").is_err());
        assert!(
            serde_json::from_str::<CatalogPage>(r#"{"data":{"items":[{"symbol":"/ESU3"}]}}"#)
                .is_err()
        );
    }
}
