use crate::{CoinbaseError, CoinbaseHistoryTransport, FixedPointValue, PublicRequestGate};
use serde::Deserialize;
use std::collections::BTreeSet;

const PAGE_SIZE: usize = 100;
const MAXIMUM_PAGES: usize = 64;
const MAXIMUM_PRODUCTS: usize = 4_096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoinbaseSpotProduct {
    pub product_id: String,
    pub instrument_id: String,
    pub display_symbol: String,
    pub base_currency: String,
    pub quote_currency: String,
    pub price_scale: u8,
    pub quantity_scale: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoinbaseCatalogDiagnostics {
    pub pages_fetched: u64,
    pub products_received: u64,
    pub inactive_products_dropped: u64,
    pub malformed_products_dropped: u64,
    pub duplicate_products_dropped: u64,
    pub truncated: bool,
}

#[derive(Deserialize)]
struct ProductsResponse {
    #[serde(default)]
    products: Vec<ProductMessage>,
    #[serde(default)]
    has_next: bool,
    #[serde(default)]
    cursor: String,
}

#[derive(Deserialize)]
struct ProductMessage {
    product_id: String,
    base_currency_id: String,
    quote_currency_id: String,
    base_increment: String,
    price_increment: String,
    #[serde(default)]
    product_type: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    trading_disabled: bool,
    #[serde(default)]
    is_disabled: bool,
    #[serde(default)]
    view_only: bool,
}

pub struct CoinbaseProductCatalog<T> {
    transport: T,
    gate: PublicRequestGate,
    diagnostics: CoinbaseCatalogDiagnostics,
}

impl<T> CoinbaseProductCatalog<T> {
    #[must_use]
    pub const fn with_transport(transport: T) -> Self {
        Self {
            transport,
            gate: PublicRequestGate::new(None),
            diagnostics: CoinbaseCatalogDiagnostics {
                pages_fetched: 0,
                products_received: 0,
                inactive_products_dropped: 0,
                malformed_products_dropped: 0,
                duplicate_products_dropped: 0,
                truncated: false,
            },
        }
    }

    #[must_use]
    pub const fn diagnostics(&self) -> CoinbaseCatalogDiagnostics {
        self.diagnostics
    }
}

impl<T: CoinbaseHistoryTransport> CoinbaseProductCatalog<T> {
    /// Fetches every bounded public catalog page and retains active spot products.
    ///
    /// Individual malformed products are skipped so one bad entry cannot discard
    /// the catalog; malformed pages, transport failures, and bound violations
    /// still fail closed. Pagination anomalies retain the validated prefix.
    ///
    /// # Errors
    ///
    /// Returns an error for transport failures or malformed pages.
    pub fn fetch_active_spot_products(
        &mut self,
    ) -> Result<Vec<CoinbaseSpotProduct>, CoinbaseError> {
        let mut cursor = None::<String>;
        let mut products = Vec::new();
        let mut identities = BTreeSet::new();
        for _ in 0..MAXIMUM_PAGES {
            let mut path =
                format!("/api/v3/brokerage/market/products?limit={PAGE_SIZE}&product_type=SPOT");
            if let Some(value) = &cursor {
                path.push_str("&cursor=");
                path.push_str(value);
            }
            let body = self
                .gate
                .get(&mut self.transport, &path)
                .map_err(CoinbaseError::Transport)?;
            let response: ProductsResponse =
                serde_json::from_slice(&body).map_err(|_| CoinbaseError::InvalidMessage)?;
            self.diagnostics.pages_fetched = self.diagnostics.pages_fetched.saturating_add(1);
            self.diagnostics.products_received = self
                .diagnostics
                .products_received
                .saturating_add(response.products.len() as u64);
            for product in response.products {
                if !is_active_spot(&product) {
                    self.diagnostics.inactive_products_dropped =
                        self.diagnostics.inactive_products_dropped.saturating_add(1);
                    continue;
                }
                let Ok(profile) = profile(product) else {
                    self.diagnostics.malformed_products_dropped = self
                        .diagnostics
                        .malformed_products_dropped
                        .saturating_add(1);
                    continue;
                };
                if !identities.insert(profile.product_id.clone()) {
                    self.diagnostics.duplicate_products_dropped = self
                        .diagnostics
                        .duplicate_products_dropped
                        .saturating_add(1);
                    continue;
                }
                if products.len() >= MAXIMUM_PRODUCTS {
                    self.diagnostics.truncated = true;
                    break;
                }
                products.push(profile);
            }
            if self.diagnostics.truncated || !response.has_next {
                products.sort_by(|left, right| {
                    product_rank(left)
                        .cmp(&product_rank(right))
                        .then_with(|| left.product_id.cmp(&right.product_id))
                });
                return Ok(products);
            }
            if response.cursor.is_empty() || cursor.as_ref() == Some(&response.cursor) {
                self.diagnostics.truncated = true;
                products.sort_by(|left, right| {
                    product_rank(left)
                        .cmp(&product_rank(right))
                        .then_with(|| left.product_id.cmp(&right.product_id))
                });
                return Ok(products);
            }
            cursor = Some(response.cursor);
        }
        self.diagnostics.truncated = true;
        products.sort_by(|left, right| {
            product_rank(left)
                .cmp(&product_rank(right))
                .then_with(|| left.product_id.cmp(&right.product_id))
        });
        Ok(products)
    }
}

fn product_rank(product: &CoinbaseSpotProduct) -> u8 {
    match product.product_id.as_str() {
        "BTC-USD" => 0,
        "ETH-USD" => 1,
        _ if product.quote_currency == "USD" => 2,
        _ if product.quote_currency == "USDC" => 3,
        _ => 4,
    }
}

fn is_active_spot(product: &ProductMessage) -> bool {
    product.product_type.eq_ignore_ascii_case("SPOT")
        && product.status.eq_ignore_ascii_case("online")
        && !product.trading_disabled
        && !product.is_disabled
        && !product.view_only
}

fn profile(product: ProductMessage) -> Result<CoinbaseSpotProduct, CoinbaseError> {
    validate_currency(&product.base_currency_id)?;
    validate_currency(&product.quote_currency_id)?;
    let expected = format!("{}-{}", product.base_currency_id, product.quote_currency_id);
    if product.product_id != expected || product.product_id.len() > 32 {
        return Err(CoinbaseError::InvalidMessage);
    }
    let price_scale = increment_scale(&product.price_increment)?;
    let quantity_scale = increment_scale(&product.base_increment)?;
    Ok(CoinbaseSpotProduct {
        instrument_id: coinbase_instrument_id(&product.product_id)?,
        display_symbol: format!("{}/{}", product.base_currency_id, product.quote_currency_id),
        product_id: product.product_id,
        base_currency: product.base_currency_id,
        quote_currency: product.quote_currency_id,
        price_scale,
        quantity_scale,
    })
}

fn validate_currency(value: &str) -> Result<(), CoinbaseError> {
    if value.is_empty()
        || value.len() > 12
        || !value
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
    {
        return Err(CoinbaseError::InvalidMessage);
    }
    Ok(())
}

fn increment_scale(value: &str) -> Result<u8, CoinbaseError> {
    let parsed = FixedPointValue::parse(value)?;
    if parsed.mantissa <= 0 || parsed.scale > 18 {
        return Err(CoinbaseError::InvalidMessage);
    }
    u8::try_from(parsed.scale).map_err(|_| CoinbaseError::InvalidMessage)
}

/// Converts a validated Coinbase product identifier into the canonical instrument identifier.
///
/// # Errors
///
/// Returns an error when the product identifier is malformed.
pub fn coinbase_instrument_id(product_id: &str) -> Result<String, CoinbaseError> {
    let (base, quote) = product_id
        .split_once('-')
        .ok_or(CoinbaseError::InvalidMessage)?;
    validate_currency(base)?;
    validate_currency(quote)?;
    Ok(format!(
        "instrument:coinbase:{}:{}",
        base.to_ascii_lowercase(),
        quote.to_ascii_lowercase()
    ))
}

#[cfg(test)]
mod tests {
    use super::{CoinbaseProductCatalog, CoinbaseSpotProduct};
    use crate::CoinbaseHistoryTransport;
    use std::collections::VecDeque;

    struct Pages(VecDeque<Vec<u8>>);

    impl CoinbaseHistoryTransport for Pages {
        fn get(&mut self, _path: &str) -> Result<Vec<u8>, String> {
            self.0.pop_front().ok_or_else(|| "missing page".to_string())
        }
    }

    #[test]
    fn catalog_paginates_and_keeps_only_active_spot_products_with_exact_precision() {
        let pages = VecDeque::from([
            br#"{"products":[{"product_id":"BTC-USD","base_currency_id":"BTC","quote_currency_id":"USD","base_increment":"0.00000001","price_increment":"0.01","product_type":"SPOT","status":"online","trading_disabled":false,"is_disabled":false,"view_only":false},{"product_id":"OLD-USD","base_currency_id":"OLD","quote_currency_id":"USD","base_increment":"0.01","price_increment":"0.01","product_type":"SPOT","status":"offline"}],"has_next":true,"cursor":"next"}"#.to_vec(),
            br#"{"products":[{"product_id":"ETH-USDC","base_currency_id":"ETH","quote_currency_id":"USDC","base_increment":"0.000001","price_increment":"0.001","product_type":"SPOT","status":"online"},{"product_id":"ETH-USD","base_currency_id":"ETH","quote_currency_id":"USD","base_increment":"0.000001","price_increment":"0.01","product_type":"SPOT","status":"online"}],"has_next":false,"cursor":""}"#.to_vec(),
        ]);
        let mut catalog = CoinbaseProductCatalog::with_transport(Pages(pages));
        let products = catalog
            .fetch_active_spot_products()
            .expect("catalog validates");
        assert_eq!(products.len(), 3);
        assert_eq!(products[0].product_id, "BTC-USD");
        assert_eq!(products[0].price_scale, 2);
        assert_eq!(products[0].quantity_scale, 8);
        assert_eq!(products[1].product_id, "ETH-USD");
        assert_eq!(products[2].instrument_id, "instrument:coinbase:eth:usdc");
        assert_eq!(catalog.diagnostics().inactive_products_dropped, 1);
    }

    #[test]
    fn catalog_skips_malformed_products_without_discarding_valid_ones() {
        let page = br#"{"products":[
            {"product_id":"BTC-USD","base_currency_id":"BTC","quote_currency_id":"USD","base_increment":"0.00000001","price_increment":"0.05","product_type":"SPOT","status":"online"},
            {"product_id":"BAD","base_currency_id":"BAD","quote_currency_id":"USD","base_increment":"0.01","price_increment":"0.01","product_type":"SPOT","status":"online"},
            {"product_id":"ETH-USD","base_currency_id":"ETH","quote_currency_id":"USD","base_increment":"0.0001","price_increment":"0.5","product_type":"SPOT","status":"online"}
        ],"has_next":false,"cursor":""}"#.to_vec();
        let mut catalog = CoinbaseProductCatalog::with_transport(Pages(VecDeque::from([page])));
        let products = catalog
            .fetch_active_spot_products()
            .expect("one malformed product cannot fail the catalog");
        assert_eq!(products.len(), 2);
        assert_eq!(products[0].product_id, "BTC-USD");
        assert_eq!(products[0].price_scale, 2);
        assert_eq!(products[1].product_id, "ETH-USD");
        assert_eq!(products[1].price_scale, 1);
        assert_eq!(products[1].quantity_scale, 4);
        assert_eq!(catalog.diagnostics().malformed_products_dropped, 1);
    }

    #[test]
    fn catalog_retains_the_validated_prefix_on_pagination_anomalies() {
        let pages = VecDeque::from([
            br#"{"products":[{"product_id":"BTC-USD","base_currency_id":"BTC","quote_currency_id":"USD","base_increment":"0.00000001","price_increment":"0.01","product_type":"SPOT","status":"online"}],"has_next":true,"cursor":"stuck"}"#.to_vec(),
            br#"{"products":[{"product_id":"ETH-USD","base_currency_id":"ETH","quote_currency_id":"USD","base_increment":"0.000001","price_increment":"0.01","product_type":"SPOT","status":"online"}],"has_next":true,"cursor":"stuck"}"#.to_vec(),
        ]);
        let mut catalog = CoinbaseProductCatalog::with_transport(Pages(pages));
        let products = catalog
            .fetch_active_spot_products()
            .expect("a repeated cursor retains validated products");
        assert_eq!(products.len(), 2);
        assert!(catalog.diagnostics().truncated);
    }

    #[test]
    fn catalog_retries_bounded_rate_limit_rejections() {
        let pages = VecDeque::from([
            b"rate limited: HTTP 429".to_vec(),
            br#"{"products":[{"product_id":"BTC-USD","base_currency_id":"BTC","quote_currency_id":"USD","base_increment":"0.00000001","price_increment":"0.01","product_type":"SPOT","status":"online"}],"has_next":false,"cursor":""}"#.to_vec(),
        ]);
        let mut catalog = CoinbaseProductCatalog::with_transport(FailingThenOk(pages));
        let products = catalog
            .fetch_active_spot_products()
            .expect("a single rate limit rejection retries");
        assert_eq!(products.len(), 1);
    }

    struct FailingThenOk(VecDeque<Vec<u8>>);

    impl CoinbaseHistoryTransport for FailingThenOk {
        fn get(&mut self, _path: &str) -> Result<Vec<u8>, String> {
            match self.0.pop_front() {
                Some(body) if body.starts_with(b"rate limited") => {
                    Err("Coinbase candle request returned HTTP 429".to_string())
                }
                Some(body) => Ok(body),
                None => Err("missing page".to_string()),
            }
        }
    }

    #[allow(dead_code)]
    fn assert_send(_: CoinbaseSpotProduct) {}
}
