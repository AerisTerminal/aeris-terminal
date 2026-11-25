//! Binance exchange-info and 24h-ticker catalog decoding.

use std::collections::{BTreeMap, HashMap};

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, decimal_places_for_step,
    parse_decimal_to_fixed,
};
use aeris_provider_spi::{
    ProviderInstrumentKind, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
};
use serde::Deserialize;

use crate::identity::{BinanceInstrument, BinanceMarketKind, instrument_id_for};

/// Retained catalog of active Binance USDT-M Perpetual and Spot instruments.
#[derive(Clone, Debug, Default)]
pub struct BinanceCatalog {
    instruments: Vec<BinanceInstrument>,
    by_provider_symbol: HashMap<String, usize>,
}

impl BinanceCatalog {
    /// Builds a catalog from decoded instruments, sorted by 24h quote volume descending.
    #[must_use]
    pub fn from_instruments(mut instruments: Vec<BinanceInstrument>) -> Self {
        instruments.sort_by(|a, b| {
            b.volume_24h_quote
                .cmp(&a.volume_24h_quote)
                .then_with(|| a.display_symbol.cmp(&b.display_symbol))
        });
        let by_provider_symbol = instruments
            .iter()
            .enumerate()
            .map(|(index, item)| (item.provider_symbol.clone(), index))
            .collect();
        Self {
            instruments,
            by_provider_symbol,
        }
    }

    /// All catalog instruments, most liquid first.
    #[must_use]
    pub fn instruments(&self) -> &[BinanceInstrument] {
        &self.instruments
    }

    /// Looks up one instrument by `provider_symbol` (e.g. `"usdm:BTCUSDT"`).
    #[must_use]
    pub fn get(&self, provider_symbol: &str) -> Option<&BinanceInstrument> {
        self.by_provider_symbol
            .get(provider_symbol)
            .and_then(|index| self.instruments.get(*index))
    }

    /// Searches the catalog by query text, returning up to `limit` matches.
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
            let base = item.base_asset.to_ascii_uppercase();
            let provider_sym = item.provider_symbol.to_ascii_uppercase();
            let inst_id = item.instrument_id.to_ascii_uppercase();
            if sym == needle
                || item.wire_symbol == needle
                || base == needle
                || provider_sym == needle
                || inst_id == needle
            {
                exact.push(item);
            } else if sym.starts_with(&needle)
                || item.wire_symbol.starts_with(&needle)
                || base.starts_with(&needle)
                || provider_sym.starts_with(&needle)
            {
                prefix.push(item);
            } else if sym.contains(&needle)
                || item.wire_symbol.contains(&needle)
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
            provider: "binance".to_string(),
            provider_generation,
            search_generation,
            instruments,
        }
    }
}

/// Projects one [`BinanceInstrument`] into a provider-neutral search summary.
#[must_use]
pub fn summary_for_instrument(item: &BinanceInstrument) -> ProviderInstrumentSummary {
    let name = match item.kind {
        BinanceMarketKind::UsdmPerp => {
            format!("{}/{} Perpetual", item.base_asset, item.quote_asset)
        }
        BinanceMarketKind::Spot => format!("{}/{} Spot", item.base_asset, item.quote_asset),
    };
    ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Instrument,
        symbol: item.provider_symbol.clone(),
        display_symbol: item.display_symbol.clone(),
        exchange: item.kind.exchange_label().to_string(),
        name: Some(name),
        product_code: Some(item.base_asset.clone()),
        instrument_type: Some(item.kind.instrument_type_label().to_string()),
        expiration_date: None,
        asset_class: Some("Crypto".to_string()),
    }
}

#[derive(Deserialize)]
struct WireExchangeInfo {
    symbols: Vec<WireSymbol>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSymbol {
    symbol: String,
    status: String,
    #[serde(default)]
    contract_type: Option<String>,
    base_asset: String,
    quote_asset: String,
    #[serde(default)]
    filters: Vec<WireFilter>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFilter {
    filter_type: String,
    #[serde(default)]
    tick_size: Option<String>,
    #[serde(default)]
    step_size: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTicker24h {
    symbol: String,
    #[serde(default)]
    quote_volume: Option<String>,
}

/// Decodes `/fapi/v1/exchangeInfo` or `/api/v3/exchangeInfo` combined with optional 24h volumes.
///
/// # Errors
/// Returns an error if the exchange-info JSON is malformed.
pub fn decode_exchange_info(
    raw: &[u8],
    kind: BinanceMarketKind,
    volumes: &BTreeMap<String, u64>,
) -> CryptoResult<Vec<BinanceInstrument>> {
    let info: WireExchangeInfo = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("binance exchangeInfo payload is malformed".to_string())
    })?;
    let mut out = Vec::new();
    for sym in info.symbols {
        if sym.status != "TRADING" {
            continue;
        }
        if kind == BinanceMarketKind::UsdmPerp && sym.contract_type.as_deref() != Some("PERPETUAL")
        {
            continue;
        }
        // Keep USDT and USDC pairs for clean, liquid markets.
        if kind == BinanceMarketKind::Spot
            && !matches!(sym.quote_asset.as_str(), "USDT" | "USDC" | "FDUSD")
        {
            continue;
        }
        let mut tick_str = "0.01";
        let mut step_str = "0.001";
        for filter in &sym.filters {
            if filter.filter_type == "PRICE_FILTER"
                && let Some(ts) = filter.tick_size.as_deref()
            {
                tick_str = ts;
            }
            if filter.filter_type == "LOT_SIZE"
                && let Some(ss) = filter.step_size.as_deref()
            {
                step_str = ss;
            }
        }
        let price_decimals = decimal_places_for_step(tick_str);
        let quantity_decimals = decimal_places_for_step(step_str);
        let tick_size_fixed = parse_decimal_to_fixed(tick_str, NORMALIZED_PRICE_SCALE)
            .ok()
            .filter(|&v| v > 0)
            .unwrap_or(1);
        let wire_symbol = sym.symbol.to_ascii_uppercase();
        let display_symbol = match kind {
            BinanceMarketKind::UsdmPerp => format!("{wire_symbol}.P"),
            BinanceMarketKind::Spot => wire_symbol.clone(),
        };
        let provider_symbol = format!("{}:{wire_symbol}", kind.prefix());
        let instrument_id = instrument_id_for(kind, &wire_symbol);
        let volume_24h_quote = volumes.get(&wire_symbol).copied().unwrap_or(0);
        out.push(BinanceInstrument {
            kind,
            wire_symbol,
            base_asset: sym.base_asset.to_ascii_uppercase(),
            quote_asset: sym.quote_asset.to_ascii_uppercase(),
            display_symbol,
            provider_symbol,
            instrument_id,
            price_decimals,
            quantity_decimals,
            tick_size_fixed,
            volume_24h_quote,
        });
    }
    Ok(out)
}

/// Decodes `/fapi/v1/ticker/24hr` or `/api/v3/ticker/24hr` into `symbol -> whole quote volume`.
#[must_use]
pub fn decode_ticker_volumes(raw: &[u8]) -> BTreeMap<String, u64> {
    let Ok(tickers) = serde_json::from_slice::<Vec<WireTicker24h>>(raw) else {
        return BTreeMap::new();
    };
    let mut map = BTreeMap::new();
    for ticker in tickers {
        if let Some(qv) = ticker.quote_volume.as_deref() {
            let whole = qv.split('.').next().unwrap_or("0");
            if let Ok(parsed) = whole.parse::<u64>() {
                map.insert(ticker.symbol.to_ascii_uppercase(), parsed);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_futures_and_spot_exchange_info() {
        let json = br#"{
            "symbols": [
                {
                    "symbol": "BTCUSDT",
                    "status": "TRADING",
                    "contractType": "PERPETUAL",
                    "baseAsset": "BTC",
                    "quoteAsset": "USDT",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.10"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.001"}
                    ]
                },
                {
                    "symbol": "BTCUSDT_260626",
                    "status": "TRADING",
                    "contractType": "CURRENT_QUARTER",
                    "baseAsset": "BTC",
                    "quoteAsset": "USDT",
                    "filters": []
                }
            ]
        }"#;
        let volumes = BTreeMap::from([("BTCUSDT".to_string(), 15_000_000_000_u64)]);
        let items = decode_exchange_info(json, BinanceMarketKind::UsdmPerp, &volumes).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].display_symbol, "BTCUSDT.P");
        assert_eq!(items[0].price_decimals, 1);
        assert_eq!(items[0].quantity_decimals, 3);
        assert_eq!(items[0].tick_size_fixed, 10_000_000);
        assert_eq!(items[0].volume_24h_quote, 15_000_000_000);
    }
}
