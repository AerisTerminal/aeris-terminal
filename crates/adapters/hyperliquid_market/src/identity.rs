//! Stable instrument identity across core perps, spot, and builder perps.
//!
//! Three strings describe every market and must never be conflated:
//! - stable `instrument_id`: engine-level identity (`hyperliquid:...`);
//! - wire `coin`: the exact `coin` value sent to the info/WebSocket API;
//! - display label: the human label shown in search, charts, and the Order Book.
//!
//! Spot markets additionally preserve pair and token indexes; builder
//! (HIP-3) perpetuals preserve their DEX namespace. Identical display names
//! across namespaces never collide because the stable id always carries the
//! namespace.

use serde::{Deserialize, Serialize};

use crate::decimal::{NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE};

/// Which Hyperliquid market class an instrument belongs to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HyperliquidMarketKind {
    /// Core perpetual namespace (`coin` is the perp name, e.g. `BTC`).
    CorePerp,
    /// Spot pair (`coin` is `PURR/USDC` or the `@<index>` wire form).
    Spot {
        /// Explicit index of the pair in the `spotMeta.universe` array.
        pair_index: u32,
        /// Token indexes within the `spotMeta.tokens` array.
        token_indexes: (u32, u32),
    },
    /// Builder-deployed (HIP-3) perpetual namespace.
    BuilderPerp {
        /// DEX namespace from `perpDexs` (e.g. `xyz` for `xyz:TSLA`).
        dex: String,
    },
}

/// Normalized Hyperliquid instrument resolved from exchange metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HyperliquidInstrument {
    /// Stable engine identity, e.g. `hyperliquid:perp:BTC`.
    pub instrument_id: String,
    /// Exact wire `coin` for info/WebSocket calls (e.g. `BTC`, `@5`, `xyz:BTC`).
    pub wire_coin: String,
    /// Provider-owned human display label. Perps use `BASE-COLLATERAL` and spot
    /// uses metadata-derived `BASE/QUOTE` (e.g. `BTC-USDC`, `HYPE/USDC`).
    pub display: String,
    /// Venue label (`Hyperliquid`, `Hyperliquid Spot`, or the DEX name).
    pub venue: String,
    /// Market class with preserved indexes/prefixes.
    pub kind: HyperliquidMarketKind,
    /// Fixed-point price scale used by the adapter (see `decimal`).
    pub price_scale: u8,
    /// Fixed-point quantity scale used by the adapter (see `decimal`).
    pub quantity_scale: u8,
    /// Native size decimals reported by the exchange.
    pub sz_decimals: u32,
}

impl HyperliquidInstrument {
    /// Validates identity, wire, display, venue, and precision fields.
    ///
    /// # Errors
    ///
    /// Returns an error for blank or overlong fields, invalid precision,
    /// or a blank builder DEX namespace.
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("instrument_id", self.instrument_id.as_str()),
            ("wire_coin", self.wire_coin.as_str()),
            ("display", self.display.as_str()),
            ("venue", self.venue.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > 256 {
                return Err(format!("hyperliquid {field} is invalid"));
            }
        }
        if self.price_scale > 18 || self.quantity_scale > 18 {
            return Err("hyperliquid instrument precision is invalid".to_string());
        }
        if let HyperliquidMarketKind::BuilderPerp { dex } = &self.kind
            && (dex.trim().is_empty() || dex.len() > 64)
        {
            return Err("hyperliquid builder dex is invalid".to_string());
        }
        Ok(())
    }
}

const LEGACY_PERP_QUOTE_ASSET: &str = "USDC";

fn perp_display(base: &str, quote_asset: &str) -> String {
    format!("{base}-{quote_asset}")
}

fn valid_perp_quote_asset(quote_asset: &str) -> bool {
    !quote_asset.is_empty()
        && quote_asset.len() <= 64
        && !quote_asset.contains(':')
        && !quote_asset.contains('/')
        && !quote_asset.contains('-')
}

/// Migrates display labels written by `Asceify`'s legacy Hyperliquid formatter
/// when the historical quote asset is unambiguous.
///
/// This is deliberately narrower than live catalog formatting. HIP-3 permits
/// arbitrary collateral, so unknown builder DEXes are never guessed here.
/// Stable identity and exact wire routing remain unchanged.
#[must_use]
pub fn legacy_display_label(
    instrument_id: &str,
    wire_coin: &str,
    display_symbol: &str,
) -> Option<String> {
    if display_symbol != format!("{wire_coin}-PERP") {
        return None;
    }
    if let Some(base) = instrument_id.strip_prefix("hyperliquid:perp:") {
        return (!base.is_empty() && base.len() <= 64 && wire_coin == base)
            .then(|| perp_display(base, LEGACY_PERP_QUOTE_ASSET));
    }

    if let Some(rest) = instrument_id.strip_prefix("hyperliquid:builder:") {
        let (dex, base) = rest.split_once(':')?;
        if dex != "xyz" {
            return None;
        }
        let expected_wire = format!("{dex}:{base}");
        return (!dex.is_empty()
            && dex.len() <= 64
            && !base.is_empty()
            && base.len() <= 64
            && !base.contains(':')
            && !base.contains('/')
            && wire_coin == expected_wire)
            .then(|| perp_display(base, LEGACY_PERP_QUOTE_ASSET));
    }

    None
}

/// Builds the stable instrument id for a market class and base name.
#[must_use]
pub fn instrument_id_for(kind: &HyperliquidMarketKind, base: &str) -> String {
    match kind {
        HyperliquidMarketKind::CorePerp => format!("hyperliquid:perp:{base}"),
        HyperliquidMarketKind::Spot { pair_index, .. } => {
            format!("hyperliquid:spot:{pair_index}:{base}")
        }
        HyperliquidMarketKind::BuilderPerp { dex } => {
            format!("hyperliquid:builder:{dex}:{base}")
        }
    }
}

/// Builds the wire coin for a market class and base name.
#[must_use]
pub fn wire_coin_for(kind: &HyperliquidMarketKind, base: &str, pair_index: Option<u32>) -> String {
    match kind {
        HyperliquidMarketKind::CorePerp => base.to_string(),
        HyperliquidMarketKind::Spot { .. } => {
            // Only PURR keeps its pair name on the wire; every other spot
            // market uses the documented `@<index>` wire form.
            if base == "PURR/USDC" {
                base.to_string()
            } else if let Some(index) = pair_index {
                format!("@{index}")
            } else {
                base.to_string()
            }
        }
        HyperliquidMarketKind::BuilderPerp { dex } => {
            // Live metadata already returns prefixed names (`xyz:TSLA`).
            if base.starts_with(&format!("{dex}:")) {
                base.to_string()
            } else {
                format!("{dex}:{base}")
            }
        }
    }
}

pub(crate) fn core_perp_with_quote(
    coin: &str,
    quote_asset: &str,
    sz_decimals: u32,
) -> Result<HyperliquidInstrument, String> {
    let coin = coin.trim();
    let quote_asset = quote_asset.trim();
    if coin.is_empty() || coin.len() > 64 || !valid_perp_quote_asset(quote_asset) {
        return Err("hyperliquid perp coin is invalid".to_string());
    }
    let kind = HyperliquidMarketKind::CorePerp;
    Ok(HyperliquidInstrument {
        instrument_id: instrument_id_for(&kind, coin),
        wire_coin: coin.to_string(),
        display: perp_display(coin, quote_asset),
        venue: "Hyperliquid".to_string(),
        kind,
        price_scale: u8::try_from(NORMALIZED_PRICE_SCALE)
            .map_err(|_| "hyperliquid instrument precision is invalid".to_string())?,
        quantity_scale: u8::try_from(NORMALIZED_QUANTITY_SCALE)
            .map_err(|_| "hyperliquid instrument precision is invalid".to_string())?,
        sz_decimals,
    })
}

pub(crate) fn builder_perp_with_quote(
    dex: &str,
    coin: &str,
    quote_asset: &str,
    sz_decimals: u32,
) -> Result<HyperliquidInstrument, String> {
    let dex = dex.trim();
    let coin = coin.trim();
    let quote_asset = quote_asset.trim();
    if dex.is_empty()
        || dex.len() > 64
        || coin.is_empty()
        || coin.len() > 96
        || !valid_perp_quote_asset(quote_asset)
    {
        return Err("hyperliquid builder market is invalid".to_string());
    }
    // Accept the documented live form (`dex:BASE`) without double-prefixing.
    let base = match coin.split_once(':') {
        Some((prefix, base)) if prefix == dex && !base.is_empty() && base.len() <= 64 => base,
        Some(_) => {
            return Err("hyperliquid builder market namespace is invalid".to_string());
        }
        None => coin,
    };
    if base.contains(':') || base.contains('/') {
        return Err("hyperliquid builder market is invalid".to_string());
    }
    let kind = HyperliquidMarketKind::BuilderPerp {
        dex: dex.to_string(),
    };
    Ok(HyperliquidInstrument {
        instrument_id: instrument_id_for(&kind, base),
        wire_coin: format!("{dex}:{base}"),
        display: perp_display(base, quote_asset),
        venue: dex.to_string(),
        kind,
        price_scale: u8::try_from(NORMALIZED_PRICE_SCALE)
            .map_err(|_| "hyperliquid instrument precision is invalid".to_string())?,
        quantity_scale: u8::try_from(NORMALIZED_QUANTITY_SCALE)
            .map_err(|_| "hyperliquid instrument precision is invalid".to_string())?,
        sz_decimals,
    })
}

/// Spot instrument for a pair name, pair index, and token indexes.
///
/// # Errors
///
/// Returns an error for blank or overlong pair names or invalid precision.
pub fn spot_pair(
    pair_name: &str,
    pair_index: u32,
    token_indexes: (u32, u32),
    sz_decimals: u32,
) -> Result<HyperliquidInstrument, String> {
    let pair_name = pair_name.trim();
    if pair_name.is_empty() || pair_name.len() > 64 {
        return Err("hyperliquid spot pair is invalid".to_string());
    }
    let kind = HyperliquidMarketKind::Spot {
        pair_index,
        token_indexes,
    };
    // Only PURR keeps its pair name on the wire; every other spot market
    // uses the documented `@<index>` wire form while the display label
    // keeps the human pair name.
    let wire_coin = if pair_name == "PURR/USDC" {
        pair_name.to_string()
    } else {
        format!("@{pair_index}")
    };
    Ok(HyperliquidInstrument {
        instrument_id: instrument_id_for(&kind, pair_name),
        wire_coin,
        display: pair_name.to_string(),
        venue: "Hyperliquid Spot".to_string(),
        kind,
        price_scale: u8::try_from(NORMALIZED_PRICE_SCALE)
            .map_err(|_| "hyperliquid instrument precision is invalid".to_string())?,
        quantity_scale: u8::try_from(NORMALIZED_QUANTITY_SCALE)
            .map_err(|_| "hyperliquid instrument precision is invalid".to_string())?,
        sz_decimals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_spot_and_builder_identities_never_collide() {
        let perp = core_perp_with_quote("BTC", LEGACY_PERP_QUOTE_ASSET, 5).expect("perp");
        let spot = spot_pair("BTC/USDC", 5, (0, 1), 8).expect("spot");
        let builder =
            builder_perp_with_quote("xyz", "BTC", LEGACY_PERP_QUOTE_ASSET, 5).expect("builder");
        assert_ne!(perp.instrument_id, spot.instrument_id);
        assert_ne!(perp.instrument_id, builder.instrument_id);
        assert_ne!(spot.instrument_id, builder.instrument_id);
        // Identical display bases in different namespaces keep distinct ids.
        let other_builder = builder_perp_with_quote("abc", "BTC", LEGACY_PERP_QUOTE_ASSET, 5)
            .expect("other builder");
        assert_ne!(builder.instrument_id, other_builder.instrument_id);
        assert_eq!(perp.wire_coin, "BTC");
        assert_eq!(spot.wire_coin, "@5");
        assert_eq!(builder.wire_coin, "xyz:BTC");
    }

    #[test]
    fn spot_pair_preserves_indexes_and_canonical_wire_form() {
        // Only PURR keeps its pair name on the wire; every other spot
        // market uses `@<index>`, including the pair at index 0.
        let purr = spot_pair("PURR/USDC", 0, (7, 0), 0).expect("purr");
        assert_eq!(purr.wire_coin, "PURR/USDC");
        let indexed = spot_pair("BTC/USDC", 1, (2, 0), 8).expect("indexed");
        assert_eq!(indexed.wire_coin, "@1");
        match &indexed.kind {
            HyperliquidMarketKind::Spot {
                pair_index,
                token_indexes,
            } => {
                assert_eq!(*pair_index, 1);
                assert_eq!(*token_indexes, (2, 0));
            }
            _ => panic!("spot kind lost"),
        }
    }

    #[test]
    fn builder_symbols_keep_live_prefixes_without_doubling() {
        // Live builder metadata already returns `dex:BASE` names.
        let live = builder_perp_with_quote("xyz", "xyz:TSLA", LEGACY_PERP_QUOTE_ASSET, 5)
            .expect("live builder");
        assert_eq!(live.wire_coin, "xyz:TSLA");
        assert_eq!(live.display, "TSLA-USDC");
        assert_eq!(live.instrument_id, "hyperliquid:builder:xyz:TSLA");
        // Unprefixed names still gain the DEX prefix.
        let plain = builder_perp_with_quote("xyz", "BTC", LEGACY_PERP_QUOTE_ASSET, 5)
            .expect("plain builder");
        assert_eq!(plain.wire_coin, "xyz:BTC");
        assert_eq!(plain.display, "BTC-USDC");
        assert_eq!(plain.instrument_id, "hyperliquid:builder:xyz:BTC");
        // A foreign namespace never silently retargets this DEX.
        assert!(builder_perp_with_quote("xyz", "abc:TSLA", LEGACY_PERP_QUOTE_ASSET, 5).is_err());
    }

    #[test]
    fn legacy_display_migration_changes_only_unambiguous_historical_perp_labels() {
        assert_eq!(
            legacy_display_label("hyperliquid:perp:BTC", "BTC", "BTC-PERP").as_deref(),
            Some("BTC-USDC")
        );
        assert_eq!(
            legacy_display_label(
                "hyperliquid:builder:xyz:XYZ100",
                "xyz:XYZ100",
                "xyz:XYZ100-PERP",
            )
            .as_deref(),
            Some("XYZ100-USDC")
        );
        assert_eq!(
            legacy_display_label("hyperliquid:builder:flx:BTC", "flx:BTC", "flx:BTC-PERP",),
            None
        );
        assert_eq!(
            legacy_display_label("hyperliquid:perp:BTC", "ETH", "ETH-PERP"),
            None
        );
        assert_eq!(
            legacy_display_label(
                "hyperliquid:builder:xyz:XYZ100",
                "xyz:XYZ100",
                "XYZ100-USDT",
            ),
            None
        );
    }

    #[test]
    fn identity_validation_rejects_blank_and_overlong_fields() {
        let mut instrument = core_perp_with_quote("BTC", LEGACY_PERP_QUOTE_ASSET, 5).expect("perp");
        instrument.validate().expect("valid");
        instrument.display.clear();
        assert!(instrument.validate().is_err());
    }
}
