//! OKX V5 instruments and 24h-ticker catalog decoding.

use std::collections::{BTreeMap, HashMap};

use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE,
    decimal_places_for_step, parse_decimal_to_fixed,
};
use aeris_provider_spi::{
    ProviderInstrumentKind, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
};
use serde::Deserialize;

use crate::identity::{OkxInstrument, OkxMarketKind, instrument_id_for};

const ONE_FIXED: i64 = 100_000_000;

/// Retained catalog of active OKX Linear Perpetual Swap and Spot instruments.
#[derive(Clone, Debug, Default)]
pub struct OkxCatalog {
    instruments: Vec<OkxInstrument>,
    by_provider_symbol: HashMap<String, usize>,
    by_inst_id: HashMap<String, usize>,
}

impl OkxCatalog {
    /// Builds a catalog sorted by 24h volume descending.
    #[must_use]
    pub fn from_instruments(mut instruments: Vec<OkxInstrument>) -> Self {
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
        let by_inst_id = instruments
            .iter()
            .enumerate()
            .map(|(index, item)| (item.inst_id.clone(), index))
            .collect();
        Self {
            instruments,
            by_provider_symbol,
            by_inst_id,
        }
    }

    /// All catalog instruments, most liquid first.
    #[must_use]
    pub fn instruments(&self) -> &[OkxInstrument] {
        &self.instruments
    }

    /// Looks up one instrument by `provider_symbol`.
    #[must_use]
    pub fn get(&self, provider_symbol: &str) -> Option<&OkxInstrument> {
        self.by_provider_symbol
            .get(provider_symbol)
            .and_then(|index| self.instruments.get(*index))
    }

    /// Looks up one instrument by native OKX `instId` (e.g. `"BTC-USDT-SWAP"`).
    #[must_use]
    pub fn get_by_inst_id(&self, inst_id: &str) -> Option<&OkxInstrument> {
        self.by_inst_id
            .get(inst_id)
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
            if sym == needle || item.inst_id == needle || base == needle {
                exact.push(item);
            } else if sym.starts_with(&needle)
                || item.inst_id.starts_with(&needle)
                || base.starts_with(&needle)
            {
                prefix.push(item);
            } else if sym.contains(&needle) || item.inst_id.contains(&needle) {
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
            provider: "okx".to_string(),
            provider_generation,
            search_generation,
            instruments,
        }
    }
}

/// Projects one [`OkxInstrument`] into a provider-neutral search summary.
#[must_use]
pub fn summary_for_instrument(item: &OkxInstrument) -> ProviderInstrumentSummary {
    let name = match item.kind {
        OkxMarketKind::Swap => format!("{}/{} Perpetual", item.base_asset, item.quote_asset),
        OkxMarketKind::Spot => format!("{}/{} Spot", item.base_asset, item.quote_asset),
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
struct WireEnvelope<T> {
    code: String,
    data: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireInstrument {
    inst_id: String,
    state: String,
    #[serde(default)]
    ct_type: String,
    #[serde(default)]
    base_ccy: String,
    #[serde(default)]
    quote_ccy: String,
    #[serde(default)]
    settle_ccy: String,
    #[serde(default)]
    ct_val_ccy: String,
    #[serde(default)]
    ct_val: String,
    #[serde(default)]
    tick_sz: String,
    #[serde(default)]
    lot_sz: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTicker {
    inst_id: String,
    #[serde(default)]
    vol_ccy24h: String,
    #[serde(default)]
    last: String,
}

/// Decodes `/api/v5/public/instruments` for `SWAP` or `SPOT`.
///
/// # Errors
/// Returns an error if the JSON is malformed or `code != "0"`.
pub fn decode_instruments(
    raw: &[u8],
    kind: OkxMarketKind,
    volumes: &BTreeMap<String, u64>,
) -> CryptoResult<Vec<OkxInstrument>> {
    let env: WireEnvelope<WireInstrument> = serde_json::from_slice(raw).map_err(|_| {
        CryptoError::MalformedPayload("okx instruments payload is malformed".to_string())
    })?;
    if env.code != "0" {
        return Err(CryptoError::Transport(format!(
            "okx instruments returned code {}",
            env.code
        )));
    }
    let mut out = Vec::new();
    for item in env.data {
        if item.state != "live" {
            continue;
        }
        let (base_asset, quote_asset, ct_val_fixed) = match kind {
            OkxMarketKind::Swap => {
                if item.ct_type != "linear" || !matches!(item.settle_ccy.as_str(), "USDT" | "USDC")
                {
                    continue;
                }
                let ct_val = parse_decimal_to_fixed(&item.ct_val, NORMALIZED_QUANTITY_SCALE)
                    .ok()
                    .filter(|&v| v > 0)
                    .unwrap_or(ONE_FIXED);
                (
                    item.ct_val_ccy.to_ascii_uppercase(),
                    item.settle_ccy.to_ascii_uppercase(),
                    ct_val,
                )
            }
            OkxMarketKind::Spot => {
                if !matches!(item.quote_ccy.as_str(), "USDT" | "USDC") {
                    continue;
                }
                (
                    item.base_ccy.to_ascii_uppercase(),
                    item.quote_ccy.to_ascii_uppercase(),
                    ONE_FIXED,
                )
            }
        };
        if base_asset.is_empty() || quote_asset.is_empty() {
            continue;
        }
        let price_decimals = decimal_places_for_step(&item.tick_sz);
        let quantity_decimals =
            decimal_places_for_step(&item.lot_sz).max(if kind == OkxMarketKind::Swap {
                decimal_places_for_step(&item.ct_val)
            } else {
                0
            });
        let tick_size_fixed = parse_decimal_to_fixed(&item.tick_sz, NORMALIZED_PRICE_SCALE)
            .ok()
            .filter(|&v| v > 0)
            .unwrap_or(1);
        let inst_id = item.inst_id.to_ascii_uppercase();
        let display_symbol = match kind {
            OkxMarketKind::Swap => format!("{base_asset}{quote_asset}.P"),
            OkxMarketKind::Spot => format!("{base_asset}{quote_asset}"),
        };
        let provider_symbol = format!("{}:{inst_id}", kind.prefix());
        let instrument_id = instrument_id_for(kind, &inst_id);
        let volume_24h_quote = volumes.get(&inst_id).copied().unwrap_or(0);
        out.push(OkxInstrument {
            kind,
            inst_id,
            base_asset,
            quote_asset,
            display_symbol,
            provider_symbol,
            instrument_id,
            price_decimals,
            quantity_decimals,
            tick_size_fixed,
            ct_val_fixed,
            volume_24h_quote,
        });
    }
    Ok(out)
}

/// Decodes `/api/v5/market/tickers` into approximate 24h quote volume per `instId`.
#[must_use]
pub fn decode_ticker_volumes(raw: &[u8], kind: OkxMarketKind) -> BTreeMap<String, u64> {
    let Ok(env) = serde_json::from_slice::<WireEnvelope<WireTicker>>(raw) else {
        return BTreeMap::new();
    };
    let mut map = BTreeMap::new();
    for ticker in env.data {
        let vol_f = ticker.vol_ccy24h.parse::<f64>().unwrap_or(0.0);
        let approx_usd = match kind {
            OkxMarketKind::Swap => {
                // For linear SWAP, volCcy24h is in base currency; multiply by last price for USD ranking.
                let last_f = ticker.last.parse::<f64>().unwrap_or(0.0);
                vol_f * last_f
            }
            OkxMarketKind::Spot => vol_f,
        };
        if approx_usd.is_finite() && approx_usd >= 0.0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let whole = approx_usd as u64;
            map.insert(ticker.inst_id.to_ascii_uppercase(), whole);
        }
    }
    map
}
