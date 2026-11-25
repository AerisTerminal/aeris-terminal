//! Coinbase Advanced Trade public product catalog decoding and search.

use std::collections::HashMap;

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, decimal_places_for_step,
    parse_decimal_to_fixed,
};
use aeris_provider_spi::{
    ProviderInstrumentKind, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
};
use serde::Deserialize;

use crate::identity::{CoinbaseInstrument, CoinbaseMarketKind, instrument_id_for};

/// Retained catalog of active Coinbase Spot instruments.
#[derive(Clone, Debug, Default)]
pub struct CoinbaseCatalog {
    instruments: Vec<CoinbaseInstrument>,
    by_provider_symbol: HashMap<String, usize>,
    by_product_id: HashMap<String, usize>,
}

impl CoinbaseCatalog {
    /// Builds a catalog sorted by 24h quote volume descending, deduplicating by `product_id`.
    #[must_use]
    pub fn from_instruments(mut instruments: Vec<CoinbaseInstrument>) -> Self {
        instruments.sort_by(|a, b| {
            b.volume_24h_quote
                .cmp(&a.volume_24h_quote)
                .then_with(|| a.display_symbol.cmp(&b.display_symbol))
        });
        let mut deduped = Vec::with_capacity(instruments.len());
        let mut by_provider_symbol = HashMap::with_capacity(instruments.len());
        let mut by_product_id = HashMap::with_capacity(instruments.len());
        for item in instruments {
            if by_product_id.contains_key(&item.product_id) {
                continue;
            }
            let index = deduped.len();
            by_provider_symbol.insert(item.provider_symbol.clone(), index);
            by_product_id.insert(item.product_id.clone(), index);
            deduped.push(item);
        }
        Self {
            instruments: deduped,
            by_provider_symbol,
            by_product_id,
        }
    }

    /// All catalog instruments, most liquid first.
    #[must_use]
    pub fn instruments(&self) -> &[CoinbaseInstrument] {
        &self.instruments
    }

    /// Looks up one instrument by `provider_symbol` (e.g. `"spot:BTC-USD"`).
    #[must_use]
    pub fn get(&self, provider_symbol: &str) -> Option<&CoinbaseInstrument> {
        self.by_provider_symbol
            .get(provider_symbol)
            .and_then(|index| self.instruments.get(*index))
    }

    /// Looks up one instrument by native Coinbase `product_id` (e.g. `"BTC-USD"`).
    #[must_use]
    pub fn get_by_product_id(&self, product_id: &str) -> Option<&CoinbaseInstrument> {
        self.by_product_id
            .get(product_id)
            .and_then(|index| self.instruments.get(*index))
    }

    /// Searches the catalog by query text.
    #[must_use]
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        consumer_id: u64,
        provider_generation: u64,
        search_generation: u64,
    ) -> ProviderInstrumentSearchResult {
        let needle = query.trim().to_ascii_uppercase();
        let cap = limit.clamp(1, 200);
        let mut exact = Vec::new();
        let mut prefix = Vec::new();
        let mut contains = Vec::new();

        for item in &self.instruments {
            if needle.is_empty() {
                contains.push(item);
                if contains.len() >= cap {
                    break;
                }
                continue;
            }
            let sym = item.display_symbol.to_ascii_uppercase();
            let compact = format!("{}{}", item.base_asset, item.quote_asset);
            let base = item.base_asset.to_ascii_uppercase();
            let provider_sym = item.provider_symbol.to_ascii_uppercase();
            let inst_id = item.instrument_id.to_ascii_uppercase();
            if sym == needle
                || compact == needle
                || item.product_id == needle
                || base == needle
                || provider_sym == needle
                || inst_id == needle
            {
                exact.push(item);
            } else if sym.starts_with(&needle)
                || compact.starts_with(&needle)
                || item.product_id.starts_with(&needle)
                || base.starts_with(&needle)
                || provider_sym.starts_with(&needle)
            {
                prefix.push(item);
            } else if sym.contains(&needle)
                || compact.contains(&needle)
                || item.product_id.contains(&needle)
                || provider_sym.contains(&needle)
            {
                contains.push(item);
            }
        }

        let instruments = exact
            .into_iter()
            .chain(prefix)
            .chain(contains)
            .take(cap)
            .map(summary_for_instrument)
            .collect();

        ProviderInstrumentSearchResult {
            consumer_id,
            provider: "coinbase".to_string(),
            provider_generation,
            search_generation,
            instruments,
        }
    }
}

/// Projects one [`CoinbaseInstrument`] into a provider-neutral search summary.
#[must_use]
pub fn summary_for_instrument(item: &CoinbaseInstrument) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Instrument,
        symbol: item.provider_symbol.clone(),
        display_symbol: item.display_symbol.clone(),
        exchange: item.kind.exchange_label().to_string(),
        name: Some(format!("{}/{} Spot", item.base_asset, item.quote_asset)),
        product_code: Some(item.base_asset.clone()),
        instrument_type: Some(item.kind.instrument_type_label().to_string()),
        expiration_date: None,
        asset_class: Some("Crypto".to_string()),
    }
}

#[derive(Deserialize)]
struct WireProductsResponse {
    #[serde(default)]
    products: Vec<WireProduct>,
    #[serde(default)]
    pagination: Option<WirePagination>,
}

#[derive(Deserialize)]
struct WirePagination {
    #[serde(default)]
    next_cursor: String,
    #[serde(default)]
    has_next: bool,
}

#[derive(Deserialize)]
struct WireProduct {
    product_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    is_disabled: bool,
    #[serde(default)]
    trading_disabled: bool,
    #[serde(default)]
    view_only: bool,
    #[serde(default)]
    product_type: String,
    #[serde(default)]
    base_currency_id: String,
    #[serde(default)]
    quote_currency_id: String,
    #[serde(default)]
    price_increment: String,
    #[serde(default)]
    quote_increment: String,
    #[serde(default)]
    base_increment: String,
    #[serde(default)]
    price: String,
    #[serde(default)]
    volume_24h: String,
    #[serde(default)]
    approximate_quote_24h_volume: String,
    #[serde(default)]
    alias: String,
    #[serde(default)]
    alias_to: Vec<String>,
}

/// Decodes one `/api/v3/brokerage/market/products` page and returns its parsed Spot instruments
/// plus an optional pagination cursor for the next page.
///
/// Alias entries (`alias != ""`) are excluded because Coinbase streams live WebSocket trades and
/// L2 order-book updates exclusively on the canonical non-alias product ID; the canonical entry
/// retains its `alias_to` list for cross-exchange aggregation (e.g. unified USD/USDC books).
///
/// # Errors
/// Returns an error if the JSON payload is malformed.
pub fn decode_products_page(raw: &[u8]) -> CryptoResult<(Vec<CoinbaseInstrument>, Option<String>)> {
    let response: WireProductsResponse = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("coinbase products payload is malformed".to_string())
    })?;
    let mut out = Vec::with_capacity(response.products.len());
    for item in response.products {
        if !item.product_type.eq_ignore_ascii_case("SPOT")
            || !item.status.eq_ignore_ascii_case("online")
            || item.is_disabled
            || item.trading_disabled
            || item.view_only
            || !item.alias.trim().is_empty()
        {
            continue;
        }
        let base_asset = item.base_currency_id.trim().to_ascii_uppercase();
        let quote_asset = item.quote_currency_id.trim().to_ascii_uppercase();
        if base_asset.is_empty() || !matches!(quote_asset.as_str(), "USD" | "USDC" | "USDT") {
            continue;
        }
        let product_id = item.product_id.trim().to_ascii_uppercase();
        if product_id.is_empty() {
            continue;
        }
        let tick_str = if item.price_increment.is_empty() {
            item.quote_increment.as_str()
        } else {
            item.price_increment.as_str()
        };
        let price_decimals = decimal_places_for_step(tick_str);
        let quantity_decimals = decimal_places_for_step(&item.base_increment);
        let tick_size_fixed = parse_decimal_to_fixed(tick_str, NORMALIZED_PRICE_SCALE)
            .ok()
            .filter(|&v| v > 0)
            .unwrap_or(1);
        let volume_24h_quote = parse_quote_volume(
            &item.approximate_quote_24h_volume,
            &item.volume_24h,
            &item.price,
        );
        let alias_to = item
            .alias_to
            .into_iter()
            .map(|entry| entry.trim().to_ascii_uppercase())
            .filter(|entry| !entry.is_empty())
            .collect();
        let kind = CoinbaseMarketKind::Spot;
        let provider_symbol = format!("{}:{product_id}", kind.prefix());
        let instrument_id = instrument_id_for(kind, &product_id);
        out.push(CoinbaseInstrument {
            kind,
            display_symbol: product_id.clone(),
            product_id,
            alias_to,
            base_asset,
            quote_asset,
            provider_symbol,
            instrument_id,
            price_decimals,
            quantity_decimals,
            tick_size_fixed,
            volume_24h_quote,
        });
    }
    let next_cursor = response
        .pagination
        .filter(|page| page.has_next && !page.next_cursor.is_empty())
        .map(|page| page.next_cursor);
    Ok((out, next_cursor))
}

fn parse_quote_volume(approx_quote: &str, base_vol: &str, price: &str) -> u64 {
    let parsed = approx_quote.parse::<f64>().ok().filter(|v| v.is_finite());
    let approx_usd = parsed.unwrap_or_else(|| {
        let vol = base_vol.parse::<f64>().unwrap_or(0.0);
        let px = price.parse::<f64>().unwrap_or(0.0);
        vol * px
    });
    if approx_usd.is_finite() && approx_usd >= 0.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = approx_usd as u64;
        whole
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_online_usd_spot_products_and_ranks_by_volume() {
        let raw = br#"{
            "products": [
                {
                    "product_id": "ETH-USD",
                    "status": "online",
                    "is_disabled": false,
                    "trading_disabled": false,
                    "view_only": false,
                    "product_type": "SPOT",
                    "base_currency_id": "ETH",
                    "quote_currency_id": "USD",
                    "price_increment": "0.01",
                    "base_increment": "0.00000001",
                    "approximate_quote_24h_volume": "250000000.5"
                },
                {
                    "product_id": "BTC-USD",
                    "status": "online",
                    "is_disabled": false,
                    "trading_disabled": false,
                    "view_only": false,
                    "product_type": "SPOT",
                    "base_currency_id": "BTC",
                    "quote_currency_id": "USD",
                    "price_increment": "0.01",
                    "base_increment": "0.00000001",
                    "approximate_quote_24h_volume": "800000000.2",
                    "alias": "",
                    "alias_to": ["BTC-USDC"]
                },
                {
                    "product_id": "BTC-USDC",
                    "status": "online",
                    "is_disabled": false,
                    "trading_disabled": false,
                    "view_only": false,
                    "product_type": "SPOT",
                    "base_currency_id": "BTC",
                    "quote_currency_id": "USDC",
                    "price_increment": "0.01",
                    "base_increment": "0.00000001",
                    "approximate_quote_24h_volume": "800000000.2",
                    "alias": "BTC-USD",
                    "alias_to": []
                },
                {
                    "product_id": "BTC-EUR",
                    "status": "online",
                    "product_type": "SPOT",
                    "base_currency_id": "BTC",
                    "quote_currency_id": "EUR",
                    "price_increment": "0.01",
                    "base_increment": "0.00000001"
                },
                {
                    "product_id": "OLD-USD",
                    "status": "delisted",
                    "product_type": "SPOT",
                    "base_currency_id": "OLD",
                    "quote_currency_id": "USD",
                    "price_increment": "0.01",
                    "base_increment": "0.01"
                }
            ],
            "pagination": {"has_next": true, "next_cursor": "cursor-2"}
        }"#;
        let (items, next) = decode_products_page(raw).expect("decode products");
        assert_eq!(next.as_deref(), Some("cursor-2"));
        assert_eq!(items.len(), 2);

        let catalog = CoinbaseCatalog::from_instruments(items);
        assert_eq!(catalog.instruments()[0].product_id, "BTC-USD");
        assert_eq!(catalog.instruments()[0].alias_to, vec!["BTC-USDC"]);
        assert_eq!(catalog.instruments()[1].product_id, "ETH-USD");
        assert!(catalog.get("spot:BTC-USDC").is_none());
        assert_eq!(
            catalog.get("spot:BTC-USD").map(|i| i.tick_size_fixed),
            Some(1_000_000)
        );
        let search = catalog.search("BTCUSD", 10, 1, 1, 1);
        assert_eq!(search.instruments.len(), 1);
        assert_eq!(search.instruments[0].symbol, "spot:BTC-USD");
    }
}
