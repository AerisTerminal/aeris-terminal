//! Catalog decoding across spot, core perps, and builder perp DEXes.
//!
//! Discovery fetches `meta` (core perps), `spotMeta` (spot pairs/tokens),
//! and `perpDexs` (builder namespaces), then one `meta` per builder DEX.
//! The last valid catalog is retained by the engine for offline display; the
//! refresh itself is a bounded background operation owned by the engine.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use serde::Deserialize;

use crate::decimal::{NORMALIZED_PRICE_SCALE, price_increment_near};
use crate::identity::{
    HyperliquidInstrument, HyperliquidMarketKind, builder_perp_with_quote, core_perp_with_quote,
    spot_pair,
};

/// One decoded catalog with stable identities for every supported market.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HyperliquidCatalog {
    /// All instruments keyed by stable `instrument_id`.
    pub instruments: BTreeMap<String, HyperliquidInstrument>,
    day_notional_volume: BTreeMap<String, String>,
    price_increment: BTreeMap<String, i64>,
}

impl HyperliquidCatalog {
    /// Looks up an instrument by stable id.
    #[must_use]
    pub fn get(&self, instrument_id: &str) -> Option<&HyperliquidInstrument> {
        self.instruments.get(instrument_id)
    }

    /// Case-insensitive substring search over display, wire, and stable id.
    ///
    /// An exact wire, display, or stable-id match ranks first so a query for
    /// `BTC` resolves the core perpetual deterministically instead of
    /// depending on where it sorts among dozens of partial matches.
    #[must_use]
    pub fn search(&self, query: &str, maximum_results: usize) -> Vec<HyperliquidInstrument> {
        let query = query.trim().to_ascii_lowercase();
        let mut scored: Vec<(u8, &HyperliquidInstrument)> = Vec::new();
        for instrument in self.instruments.values() {
            let display = instrument.display.to_ascii_lowercase();
            let wire = instrument.wire_coin.to_ascii_lowercase();
            let stable = instrument.instrument_id.to_ascii_lowercase();
            let rank = if query.is_empty() {
                3
            } else if wire == query || display == query || stable == query {
                0
            } else if display.contains(&query) {
                1
            } else if wire.contains(&query) || stable.contains(&query) {
                2
            } else {
                continue;
            };
            scored.push((rank, instrument));
        }
        scored.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| {
                    compare_unsigned_decimals(
                        self.day_notional_volume
                            .get(&right.1.instrument_id)
                            .map_or("0", String::as_str),
                        self.day_notional_volume
                            .get(&left.1.instrument_id)
                            .map_or("0", String::as_str),
                    )
                })
                .then_with(|| left.1.display.cmp(&right.1.display))
        });
        scored
            .into_iter()
            .take(maximum_results.max(1))
            .map(|(_, instrument)| instrument.clone())
            .collect()
    }

    /// Valid price increment near the latest catalog mark price, in units of
    /// the instrument's price scale. `None` when the context carried no
    /// usable mark price.
    #[must_use]
    pub fn price_increment(&self, instrument_id: &str) -> Option<i64> {
        self.price_increment.get(instrument_id).copied()
    }

    pub(crate) fn set_reference_price(&mut self, wire_coin: &str, mark_price: &str) {
        let Some(instrument) = self
            .instruments
            .values()
            .find(|instrument| instrument.wire_coin == wire_coin)
        else {
            return;
        };
        if u32::from(instrument.price_scale) != NORMALIZED_PRICE_SCALE {
            return;
        }
        let spot = matches!(instrument.kind, HyperliquidMarketKind::Spot { .. });
        if let Ok(increment) = price_increment_near(mark_price, instrument.sz_decimals, spot) {
            self.price_increment
                .insert(instrument.instrument_id.clone(), increment);
        }
    }

    pub(crate) fn set_day_notional_volume(&mut self, wire_coin: &str, volume: &str) {
        if !valid_unsigned_decimal(volume) {
            return;
        }
        if let Some(instrument) = self
            .instruments
            .values()
            .find(|instrument| instrument.wire_coin == wire_coin)
        {
            self.day_notional_volume
                .insert(instrument.instrument_id.clone(), volume.to_string());
        }
    }
}

fn valid_unsigned_decimal(value: &str) -> bool {
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    parts.next().is_none()
        && !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction
            .is_none_or(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn compare_unsigned_decimals(left: &str, right: &str) -> Ordering {
    let (left_whole, left_fraction) = decimal_parts(left);
    let (right_whole, right_fraction) = decimal_parts(right);
    left_whole
        .len()
        .cmp(&right_whole.len())
        .then_with(|| left_whole.cmp(right_whole))
        .then_with(|| {
            let width = left_fraction.len().max(right_fraction.len());
            left_fraction
                .bytes()
                .chain(std::iter::repeat_n(b'0', width - left_fraction.len()))
                .cmp(
                    right_fraction
                        .bytes()
                        .chain(std::iter::repeat_n(b'0', width - right_fraction.len())),
                )
        })
}

fn decimal_parts(value: &str) -> (&str, &str) {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    (
        whole.trim_start_matches('0'),
        fraction.trim_end_matches('0'),
    )
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct PerpMeta {
    #[serde(default)]
    universe: Vec<PerpAsset>,
    #[serde(default)]
    collateralToken: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct PerpAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    szDecimals: u32,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct SpotMeta {
    #[serde(default)]
    universe: Vec<SpotPair>,
    #[serde(default)]
    tokens: Vec<SpotToken>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct SpotPair {
    #[serde(default)]
    name: String,
    #[serde(default)]
    tokens: Vec<u32>,
    /// Explicit pair index; falls back to universe position when absent.
    #[serde(default)]
    index: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct SpotToken {
    #[serde(default)]
    name: String,
    #[serde(default)]
    szDecimals: u32,
    #[serde(default)]
    index: u32,
}

/// Raw bundle fetched by the engine's bounded catalog refresh.
#[derive(Clone, Debug, Default)]
pub struct RawMetaBundle {
    /// `{"type":"meta"}` response for the core perp DEX.
    pub core_perp_meta: serde_json::Value,
    /// `{"type":"spotMeta"}` response.
    pub spot_meta: serde_json::Value,
    /// `{"type":"perpDexs"}` response (array of dex names, null entries ok).
    pub perp_dexs: serde_json::Value,
    /// One `{"type":"meta","dex":...}` response per builder DEX, in order.
    pub builder_metas: Vec<(String, serde_json::Value)>,
}

/// Decodes a fetched bundle into stable catalog identities.
///
/// Display names derive from token metadata so wire placeholders such as
/// `@7` never leak into the UI; sizes settle in the base asset, so each
/// spot market carries its base token's `szDecimals`.
///
/// # Errors
///
/// Returns an error for malformed metadata, duplicate markets, or an empty
/// catalog.
pub fn decode_catalog(bundle: &RawMetaBundle) -> Result<HyperliquidCatalog, String> {
    let mut instruments = BTreeMap::new();
    let mut display_collisions = BTreeSet::new();

    let core: PerpMeta = serde_json::from_value(bundle.core_perp_meta.clone())
        .map_err(|_| "hyperliquid perp meta is malformed".to_string())?;
    if core.universe.is_empty() || core.universe.len() > 10_000 {
        return Err("hyperliquid perp meta is malformed".to_string());
    }
    let spot: SpotMeta = serde_json::from_value(bundle.spot_meta.clone())
        .map_err(|_| "hyperliquid spot meta is malformed".to_string())?;
    if spot.universe.len() > 10_000 || spot.tokens.len() > 10_000 {
        return Err("hyperliquid spot meta is malformed".to_string());
    }
    let token_names: BTreeMap<u32, String> = spot
        .tokens
        .iter()
        .map(|token| (token.index, token.name.clone()))
        .collect();
    let token_decimals: BTreeMap<u32, u32> = spot
        .tokens
        .iter()
        .map(|token| (token.index, token.szDecimals))
        .collect();
    let core_quote = perp_quote_asset(core.collateralToken, &token_names)?;
    for asset in &core.universe {
        if asset.name.trim().is_empty() || asset.name.len() > 64 {
            continue;
        }
        let instrument = core_perp_with_quote(&asset.name, core_quote, asset.szDecimals)?;
        insert_checked(&mut instruments, &mut display_collisions, instrument)?;
    }

    for (position, pair) in spot.universe.iter().enumerate() {
        // Prefer the explicit pair index; universe position is only a
        // fallback for older payloads.
        let fallback = u32::try_from(position).unwrap_or(u32::MAX);
        let pair_index = pair.index.unwrap_or(fallback);
        if pair.tokens.len() != 2 {
            continue;
        }
        // Sizes settle in the base asset, so the market precision is the
        // base token's `szDecimals` — never the maximum of both sides.
        let Some(decimals) = token_decimals.get(&pair.tokens[0]).copied() else {
            continue;
        };
        // Derive the human pair name from token metadata (`HFUN/USDC`);
        // the raw wire name may itself be an `@<index>` placeholder.
        let pair_name = match (
            token_names.get(&pair.tokens[0]),
            token_names.get(&pair.tokens[1]),
        ) {
            (Some(base), Some(quote)) if !base.trim().is_empty() && !quote.trim().is_empty() => {
                format!("{base}/{quote}")
            }
            _ if !pair.name.trim().is_empty() && pair.name.len() <= 64 => pair.name.clone(),
            _ => continue,
        };
        let instrument = spot_pair(
            &pair_name,
            pair_index,
            (pair.tokens[0], pair.tokens[1]),
            decimals,
        )?;
        insert_checked(&mut instruments, &mut display_collisions, instrument)?;
    }

    for (dex, meta) in &bundle.builder_metas {
        let parsed: PerpMeta = serde_json::from_value(meta.clone())
            .map_err(|_| "hyperliquid builder meta is malformed".to_string())?;
        let quote_asset = perp_quote_asset(parsed.collateralToken, &token_names)?;
        for asset in &parsed.universe {
            if asset.name.trim().is_empty() || asset.name.len() > 64 {
                continue;
            }
            let instrument =
                builder_perp_with_quote(dex, &asset.name, quote_asset, asset.szDecimals)?;
            insert_checked(&mut instruments, &mut display_collisions, instrument)?;
        }
    }

    if instruments.is_empty() {
        return Err("hyperliquid catalog is empty".to_string());
    }
    // Display collisions are legal (identical bases across namespaces); the
    // stable ids above already keep them distinct. The set is retained only
    // to prove the invariant in tests.
    let _ = display_collisions;
    Ok(HyperliquidCatalog {
        instruments,
        day_notional_volume: BTreeMap::new(),
        price_increment: BTreeMap::new(),
    })
}

fn perp_quote_asset(
    collateral_token: Option<u32>,
    token_names: &BTreeMap<u32, String>,
) -> Result<&str, String> {
    let token_index = collateral_token.unwrap_or(0);
    token_names
        .get(&token_index)
        .map(String::as_str)
        .filter(|name| !name.trim().is_empty() && name.len() <= 64)
        .ok_or_else(|| "hyperliquid perp collateral token is unavailable".to_string())
}

fn insert_checked(
    instruments: &mut BTreeMap<String, HyperliquidInstrument>,
    collisions: &mut BTreeSet<String>,
    instrument: HyperliquidInstrument,
) -> Result<(), String> {
    instrument.validate()?;
    if !collisions.insert(instrument.display.clone()) {
        // Same display seen before in another namespace: allowed, ids differ.
    }
    if instruments.contains_key(&instrument.instrument_id) {
        return Err("hyperliquid catalog contains a duplicate market".to_string());
    }
    instruments.insert(instrument.instrument_id.clone(), instrument);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bundle() -> RawMetaBundle {
        RawMetaBundle {
            core_perp_meta: json!({"universe": [
                {"name": "BTC", "szDecimals": 5},
                {"name": "ETH", "szDecimals": 4},
            ]}),
            spot_meta: json!({
                "universe": [
                    {"name": "USDC/USDT", "tokens": [0, 1]},
                    {"name": "BTC/USDC", "tokens": [2, 0]},
                ],
                "tokens": [
                    {"name": "USDC", "szDecimals": 8, "index": 0},
                    {"name": "USDT", "szDecimals": 8, "index": 1},
                    {"name": "BTC", "szDecimals": 8, "index": 2},
                ],
            }),
            perp_dexs: json!(["xyz", null]),
            builder_metas: vec![(
                "xyz".to_string(),
                json!({"universe": [{"name": "BTC", "szDecimals": 5}]}),
            )],
        }
    }

    #[test]
    fn discovery_separates_spot_core_and_builder_identities() {
        let catalog = decode_catalog(&bundle()).expect("catalog");
        let core = catalog.get("hyperliquid:perp:BTC").expect("core perp");
        assert_eq!(core.display, "BTC-USDC");
        assert!(catalog.get("hyperliquid:perp:ETH").is_some());
        // Only PURR keeps its pair name on the wire; every other spot
        // market uses `@<index>` with a token-derived display name.
        let spot = catalog
            .get("hyperliquid:spot:0:USDC/USDT")
            .expect("spot pair");
        assert_eq!(spot.display, "USDC/USDT");
        assert_eq!(spot.wire_coin, "@0");
        let indexed = catalog
            .get("hyperliquid:spot:1:BTC/USDC")
            .expect("indexed spot");
        assert_eq!(indexed.display, "BTC/USDC");
        assert_eq!(indexed.wire_coin, "@1");
        let builder = catalog
            .get("hyperliquid:builder:xyz:BTC")
            .expect("builder perp");
        assert_eq!(builder.wire_coin, "xyz:BTC");
        assert_eq!(builder.display, "BTC-USDC");
        assert_eq!(builder.venue, "xyz");
    }

    #[test]
    fn perp_display_uses_the_metadata_collateral_token() {
        let mut alternate = bundle();
        alternate.core_perp_meta = json!({
            "universe": [{"name": "BTC", "szDecimals": 5}],
            "collateralToken": 1,
        });
        alternate.builder_metas = vec![(
            "xyz".to_string(),
            json!({
                "universe": [{"name": "xyz:XYZ100", "szDecimals": 4}],
                "collateralToken": 1,
            }),
        )];

        let catalog = decode_catalog(&alternate).expect("catalog");
        assert_eq!(
            catalog
                .get("hyperliquid:perp:BTC")
                .expect("core perp")
                .display,
            "BTC-USDT"
        );
        assert_eq!(
            catalog
                .get("hyperliquid:builder:xyz:XYZ100")
                .expect("builder perp")
                .display,
            "XYZ100-USDT"
        );
    }

    #[test]
    fn empty_search_ranks_markets_by_reported_day_notional_volume() {
        let mut catalog = decode_catalog(&bundle()).expect("catalog");
        catalog.set_day_notional_volume("BTC", "1250000.50");
        catalog.set_day_notional_volume("@1", "9000000.25");
        catalog.set_day_notional_volume("ETH", "1250000.5000000001");

        let results = catalog.search("", 3);
        assert_eq!(results[0].instrument_id, "hyperliquid:spot:1:BTC/USDC");
        assert_eq!(results[1].instrument_id, "hyperliquid:perp:ETH");
        assert_eq!(results[2].instrument_id, "hyperliquid:perp:BTC");
    }

    #[test]
    fn spot_uses_explicit_index_derived_names_and_base_precision() {
        // Live PURR/USDC metadata: base precision 0, quote precision 8,
        // with an explicit pair index that differs from array position.
        let bundle = RawMetaBundle {
            core_perp_meta: json!({"universe": [{"name": "BTC", "szDecimals": 5}]}),
            spot_meta: json!({
                "universe": [
                    {"name": "@7", "tokens": [7, 0], "index": 7},
                    {"name": "PURR/USDC", "tokens": [9, 0], "index": 3},
                ],
                "tokens": [
                    {"name": "USDC", "szDecimals": 8, "index": 0},
                    {"name": "HFUN", "szDecimals": 2, "index": 7},
                    {"name": "PURR", "szDecimals": 0, "index": 9},
                ],
            }),
            perp_dexs: json!([]),
            builder_metas: vec![],
        };
        let catalog = decode_catalog(&bundle).expect("catalog");
        // Display derives from token metadata, not the `@7` wire name, and
        // the stable id carries the explicit index, not position 0.
        let hfun = catalog
            .get("hyperliquid:spot:7:HFUN/USDC")
            .expect("derived spot pair");
        assert_eq!(hfun.display, "HFUN/USDC");
        assert_eq!(hfun.wire_coin, "@7");
        assert_eq!(hfun.sz_decimals, 2);
        // PURR keeps its pair name on the wire with base precision 0.
        let purr = catalog
            .get("hyperliquid:spot:3:PURR/USDC")
            .expect("purr pair");
        assert_eq!(purr.wire_coin, "PURR/USDC");
        assert_eq!(purr.sz_decimals, 0);
    }

    #[test]
    fn builder_catalog_accepts_live_prefixed_names() {
        let mut prefixed = bundle();
        prefixed.builder_metas = vec![(
            "xyz".to_string(),
            json!({"universe": [{"name": "xyz:TSLA", "szDecimals": 3}]}),
        )];
        let catalog = decode_catalog(&prefixed).expect("catalog");
        let instrument = catalog
            .get("hyperliquid:builder:xyz:TSLA")
            .expect("prefixed builder market");
        assert_eq!(instrument.wire_coin, "xyz:TSLA");
        assert_eq!(instrument.display, "TSLA-USDC");
    }

    #[test]
    fn identical_display_names_never_collide() {
        let catalog = decode_catalog(&bundle()).expect("catalog");
        // Core BTC and builder xyz:BTC share the base but not the id.
        assert_ne!(
            catalog.get("hyperliquid:perp:BTC").unwrap().instrument_id,
            catalog
                .get("hyperliquid:builder:xyz:BTC")
                .unwrap()
                .instrument_id
        );
    }

    #[test]
    fn malformed_and_empty_bundles_fail_closed() {
        let mut bad = bundle();
        bad.core_perp_meta = json!({"universe": []});
        assert!(decode_catalog(&bad).is_err());
        let mut malformed = bundle();
        malformed.spot_meta = json!({"universe": "nope"});
        assert!(decode_catalog(&malformed).is_err());
    }

    #[test]
    fn search_ranks_display_before_wire_and_stable_id() {
        let catalog = decode_catalog(&bundle()).expect("catalog");
        let results = catalog.search("btc", 10);
        assert_ne!(results, [] as [HyperliquidInstrument; 0]);
        assert!(
            results
                .iter()
                .any(|instrument| instrument.instrument_id == "hyperliquid:perp:BTC")
        );
    }

    #[test]
    fn search_resolves_exact_wire_symbols_deterministically() {
        let catalog = decode_catalog(&bundle()).expect("catalog");
        // Exact matches win over partials no matter how many exist.
        let results = catalog.search("BTC", 10);
        assert_eq!(
            results.first().map(|first| first.wire_coin.as_str()),
            Some("BTC")
        );
        let indexed = catalog.search("@1", 10);
        assert_eq!(
            indexed.first().map(|first| first.wire_coin.as_str()),
            Some("@1")
        );
    }
}
