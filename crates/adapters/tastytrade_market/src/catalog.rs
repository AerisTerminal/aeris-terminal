//! Provider metadata for bounded, read-only futures discovery.

use serde::Deserialize;

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
