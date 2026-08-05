//! Instrument identity for Coinbase spot products.
//!
//! Internal instrument identity is provider-neutral (Section 10.3); the
//! Coinbase product string stays a provider mapping. Spot USD pairs use
//! Coinbase's own increments: price scale 2 (quote increment 0.01) and
//! quantity scale 8 (base increment 1e-8).

use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::BarDefinition;

/// One product mapping from the internal instrument to the Coinbase product.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductMapping {
    pub instrument: InstrumentRevision,
    pub bar_definition: BarDefinition,
    pub coinbase_product: String,
    pub price_scale: u8,
    pub quantity_scale: u8,
}

/// Maps one Coinbase spot USD product to its internal instrument.
///
/// # Errors
///
/// Returns an error for malformed products or unsupported quote currencies.
pub fn map_product(coinbase_product: &str) -> Result<ProductMapping, String> {
    let (base, quote) = coinbase_product
        .split_once('-')
        .ok_or_else(|| format!("malformed Coinbase product: {coinbase_product}"))?;
    if quote != "USD" || base.is_empty() || base.len() > 16 {
        return Err(format!("unsupported Coinbase product: {coinbase_product}"));
    }
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(format!(
            "instrument:coinbase:{}:usd",
            base.to_ascii_lowercase()
        ))
        .map_err(|error| error.to_string())?,
        revision: 1,
        asset_class: AssetClass::CryptoAsset,
        symbol: base.to_string(),
        venue_id: "COINBASE".to_string(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(2, 8).map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    };
    instrument.validate().map_err(|error| error.to_string())?;
    let bar_definition = BarDefinition {
        definition_id: "coinbase:one_minute:spot:v1".to_string(),
        version: 1,
        interval_seconds: 60,
    };
    bar_definition
        .validate()
        .map_err(|error| error.to_string())?;
    Ok(ProductMapping {
        instrument,
        bar_definition,
        coinbase_product: coinbase_product.to_string(),
        price_scale: 2,
        quantity_scale: 8,
    })
}

#[cfg(test)]
mod tests {
    use super::map_product;

    #[test]
    fn btc_usd_maps_to_a_crypto_asset_instrument() {
        let mapping = map_product("BTC-USD").expect("BTC-USD maps");
        assert_eq!(mapping.instrument.symbol, "BTC");
        assert_eq!(mapping.instrument.venue_id, "COINBASE");
        assert_eq!(mapping.bar_definition.interval_seconds, 60);
    }
}
