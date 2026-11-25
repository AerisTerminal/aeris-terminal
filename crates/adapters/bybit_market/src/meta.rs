//! Bybit V5 instruments-info and 24h-ticker catalog decoding.

use std::collections::{BTreeMap, HashMap};

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, decimal_places_for_step,
    parse_decimal_to_fixed,
};
use aeris_provider_spi::{
    ProviderInstrumentKind, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
};
use serde::Deserialize;

use crate::identity::{BybitInstrument, BybitMarketKind, instrument_id_for};

/// Retained catalog of active Bybit Linear Perpetual and Spot instruments.
#[derive(Clone, Debug, Default)]
pub struct BybitCatalog {
    instruments: Vec<BybitInstrument>,
    by_provider_symbol: HashMap<String, usize>,
}

impl BybitCatalog {
    /// Builds a catalog sorted by 24h turnover descending.
    #[must_use]
    pub fn from_instruments(mut instruments: Vec<BybitInstrument>) -> Self {
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
    pub fn instruments(&self) -> &[BybitInstrument] {
        &self.instruments
    }

    /// Looks up one instrument by `provider_symbol`.
    #[must_use]
    pub fn get(&self, provider_symbol: &str) -> Option<&BybitInstrument> {
        self.by_provider_symbol
            .get(provider_symbol)
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
            provider: "bybit".to_string(),
            provider_generation,
            search_generation,
            instruments,
        }
    }
}

/// Projects one [`BybitInstrument`] into a provider-neutral search summary.
#[must_use]
pub fn summary_for_instrument(item: &BybitInstrument) -> ProviderInstrumentSummary {
    let name = match item.kind {
        BybitMarketKind::Linear => format!("{}/{} Perpetual", item.base_asset, item.quote_asset),
        BybitMarketKind::Spot => format!("{}/{} Spot", item.base_asset, item.quote_asset),
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
#[serde(rename_all = "camelCase")]
struct WireEnvelope<T> {
    ret_code: i64,
    result: T,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireInstrumentsResult {
    list: Vec<WireInstrument>,
    #[serde(default)]
    next_page_cursor: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireInstrument {
    symbol: String,
    #[serde(default)]
    contract_type: Option<String>,
    status: String,
    base_coin: String,
    quote_coin: String,
    #[serde(default)]
    price_filter: Option<WirePriceFilter>,
    #[serde(default)]
    lot_size_filter: Option<WireLotSizeFilter>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePriceFilter {
    #[serde(default)]
    tick_size: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireLotSizeFilter {
    #[serde(default)]
    qty_step: Option<String>,
    #[serde(default)]
    base_precision: Option<String>,
}

#[derive(Deserialize)]
struct WireTickersResult {
    list: Vec<WireTicker>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTicker {
    symbol: String,
    #[serde(default)]
    turnover24h: Option<String>,
}

/// Decodes one page of `/v5/market/instruments-info`, returning `(instruments, next_cursor)`.
///
/// # Errors
/// Returns an error if the JSON is malformed or `retCode != 0`.
pub fn decode_instruments_page(
    raw: &[u8],
    kind: BybitMarketKind,
    volumes: &BTreeMap<String, u64>,
) -> CryptoResult<(Vec<BybitInstrument>, Option<String>)> {
    let envelope: WireEnvelope<WireInstrumentsResult> =
        serde_json::from_slice(raw).map_err(|_| {
            CryptoError::MalformedPayload("bybit instruments-info payload is malformed".to_string())
        })?;
    if envelope.ret_code != 0 {
        return Err(CryptoError::Transport(format!(
            "bybit instruments-info returned retCode {}",
            envelope.ret_code
        )));
    }
    let mut out = Vec::new();
    for item in envelope.result.list {
        if item.status != "Trading" {
            continue;
        }
        if kind == BybitMarketKind::Linear
            && item.contract_type.as_deref() != Some("LinearPerpetual")
        {
            continue;
        }
        if kind == BybitMarketKind::Spot && !matches!(item.quote_coin.as_str(), "USDT" | "USDC") {
            continue;
        }
        let tick_str = item
            .price_filter
            .as_ref()
            .and_then(|f| f.tick_size.as_deref())
            .unwrap_or("0.01");
        let step_str = item
            .lot_size_filter
            .as_ref()
            .and_then(|f| f.qty_step.as_deref().or(f.base_precision.as_deref()))
            .unwrap_or("0.001");
        let price_decimals = decimal_places_for_step(tick_str);
        let quantity_decimals = decimal_places_for_step(step_str);
        let tick_size_fixed = parse_decimal_to_fixed(tick_str, NORMALIZED_PRICE_SCALE)
            .ok()
            .filter(|&v| v > 0)
            .unwrap_or(1);
        let wire_symbol = item.symbol.to_ascii_uppercase();
        let display_symbol = match kind {
            BybitMarketKind::Linear => format!("{wire_symbol}.P"),
            BybitMarketKind::Spot => wire_symbol.clone(),
        };
        let provider_symbol = format!("{}:{wire_symbol}", kind.category());
        let instrument_id = instrument_id_for(kind, &wire_symbol);
        let volume_24h_quote = volumes.get(&wire_symbol).copied().unwrap_or(0);
        out.push(BybitInstrument {
            kind,
            wire_symbol,
            base_asset: item.base_coin.to_ascii_uppercase(),
            quote_asset: item.quote_coin.to_ascii_uppercase(),
            display_symbol,
            provider_symbol,
            instrument_id,
            price_decimals,
            quantity_decimals,
            tick_size_fixed,
            volume_24h_quote,
        });
    }
    let next_cursor =
        (!envelope.result.next_page_cursor.is_empty()).then_some(envelope.result.next_page_cursor);
    Ok((out, next_cursor))
}

/// Decodes `/v5/market/tickers` into `symbol -> whole 24h turnover`.
#[must_use]
pub fn decode_ticker_volumes(raw: &[u8]) -> BTreeMap<String, u64> {
    let Ok(envelope) = serde_json::from_slice::<WireEnvelope<WireTickersResult>>(raw) else {
        return BTreeMap::new();
    };
    let mut map = BTreeMap::new();
    for ticker in envelope.result.list {
        if let Some(turnover) = ticker.turnover24h.as_deref() {
            let whole = turnover.split('.').next().unwrap_or("0");
            if let Ok(parsed) = whole.parse::<u64>() {
                map.insert(ticker.symbol.to_ascii_uppercase(), parsed);
            }
        }
    }
    map
}
