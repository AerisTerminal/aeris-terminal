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

/// Converts an adapter fixed-point value to the instrument scale as a bare
/// mantissa, rejecting any value that would lose precision or overflow.
///
/// # Errors
///
/// Returns an error for precision loss or overflow.
pub fn mantissa_at_scale(
    value: axiusflow_coinbase_market_adapter::FixedPointValue,
    target_scale: u8,
) -> Result<i64, String> {
    let source = i128::from(value.mantissa);
    let scaled = if value.scale <= u32::from(target_scale) {
        let shift = u32::from(target_scale) - value.scale;
        source
            .checked_mul(10_i128.pow(shift))
            .ok_or_else(|| "fixed-point conversion overflow".to_string())?
    } else {
        let shift = value.scale - u32::from(target_scale);
        let divisor = 10_i128.pow(shift);
        if source % divisor != 0 {
            return Err("fixed-point conversion would lose precision".to_string());
        }
        source / divisor
    };
    i64::try_from(scaled).map_err(|_| "fixed-point conversion overflow".to_string())
}

#[cfg(test)]
mod tests {
    use super::{mantissa_at_scale, map_product};
    use axiusflow_coinbase_market_adapter::FixedPointValue;

    #[test]
    fn btc_usd_maps_to_a_crypto_asset_instrument() {
        let mapping = map_product("BTC-USD").expect("BTC-USD maps");
        assert_eq!(mapping.instrument.symbol, "BTC");
        assert_eq!(mapping.instrument.venue_id, "COINBASE");
        assert_eq!(mapping.bar_definition.interval_seconds, 60);
    }

    #[test]
    fn fixed_point_scales_exactly() {
        let price = FixedPointValue::parse("67001.25").expect("valid");
        assert_eq!(mantissa_at_scale(price, 2).expect("exact"), 6_700_125);
        let size = FixedPointValue::parse("0.00000001").expect("valid");
        assert_eq!(mantissa_at_scale(size, 8).expect("exact"), 1);
        let fractional = FixedPointValue::parse("1.234").expect("valid");
        assert!(mantissa_at_scale(fractional, 2).is_err());
    }
}
