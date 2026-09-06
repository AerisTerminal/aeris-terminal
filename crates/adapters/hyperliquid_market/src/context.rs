//! Public market context kept separate from traded prices and candles.
//!
//! Mark/oracle prices, funding, open interest, and day volume are display
//! context only. They never enter candle construction and missing fields for
//! a market class are never fabricated (spot has no funding or open
//! interest, for example).
//!
//! Each field uses its own explicit scale: prices at 8 places (spot allows
//! up to 8 price decimals), funding rates and notional volumes at 10 places
//! for live values such as funding `0.0000125` and day notional
//! `785361336.8372405767`. Wire values arrive as strings or JSON numbers.

use serde::Deserialize;

use crate::decimal::{
    FUNDING_RATE_SCALE, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE, NOTIONAL_SCALE,
    RawDecimal,
};

/// Display-only market context for one instrument.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HyperliquidMarketContext {
    /// Mark price in normalized fixed point (8 places), when supplied.
    pub mark_price: Option<i64>,
    /// Oracle price in normalized fixed point (8 places), when supplied.
    pub oracle_price: Option<i64>,
    /// Mid price in normalized fixed point (8 places), when supplied.
    pub mid_price: Option<i64>,
    /// Current funding rate in normalized fixed point (10 places), when supplied.
    pub funding_rate: Option<i64>,
    /// Open interest in normalized fixed quantity (8 places), when supplied.
    pub open_interest: Option<i64>,
    /// Day notional volume in normalized fixed notional (10 places), when supplied.
    pub day_volume: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[allow(non_snake_case)]
struct WireAssetCtx {
    #[serde(default)]
    markPx: Option<RawDecimal>,
    #[serde(default)]
    oraclePx: Option<RawDecimal>,
    #[serde(default)]
    midPx: Option<RawDecimal>,
    #[serde(default)]
    funding: Option<RawDecimal>,
    #[serde(default)]
    openInterest: Option<RawDecimal>,
    #[serde(default)]
    dayNtlVlm: Option<RawDecimal>,
}

/// Live subscription envelope: context fields ride inside `ctx`.
#[derive(Debug, Deserialize)]
struct WireCtxEnvelope {
    #[serde(default)]
    ctx: Option<Box<serde_json::value::RawValue>>,
}

/// Decodes one `metaAndAssetCtxs`/`activeAssetCtx` context object.
///
/// The payload arrives as raw JSON text so fractional numbers never pass
/// through `f64`. Live `activeAssetCtx` frames wrap the context in a
/// `{coin, ctx}` envelope; the envelope's `ctx` contents are what decode —
/// never the envelope itself, which would silently yield an empty context.
/// Bare context objects (REST `metaAndAssetCtxs` entries) decode directly.
///
/// Every field is optional: absent fields stay `None` and are never
/// fabricated for a market class that does not provide them.
///
/// # Errors
///
/// Returns an error for malformed payloads, envelope shapes without usable
/// `ctx` contents, or invalid decimal fields.
pub fn decode_asset_context(
    payload: &serde_json::value::RawValue,
) -> Result<HyperliquidMarketContext, String> {
    let routing: serde_json::Value = serde_json::from_str(payload.get())
        .map_err(|_| "hyperliquid market context is malformed".to_string())?;
    // A top-level `coin` key marks the live subscription envelope; bare
    // context objects never carry one.
    if routing.get("coin").is_some() {
        let envelope: WireCtxEnvelope = serde_json::from_str(payload.get())
            .map_err(|_| "hyperliquid market context is malformed".to_string())?;
        let Some(ctx) = envelope.ctx else {
            return Err("hyperliquid market context is malformed".to_string());
        };
        return decode_ctx_object(&ctx);
    }
    decode_ctx_object(payload)
}

fn decode_ctx_object(
    payload: &serde_json::value::RawValue,
) -> Result<HyperliquidMarketContext, String> {
    let wire: WireAssetCtx = serde_json::from_str(payload.get())
        .map_err(|_| "hyperliquid market context is malformed".to_string())?;
    let fixed =
        |value: Option<RawDecimal>, scale: u32| value.map(|raw| raw.to_fixed(scale)).transpose();
    Ok(HyperliquidMarketContext {
        mark_price: fixed(wire.markPx, NORMALIZED_PRICE_SCALE)?,
        oracle_price: fixed(wire.oraclePx, NORMALIZED_PRICE_SCALE)?,
        mid_price: fixed(wire.midPx, NORMALIZED_PRICE_SCALE)?,
        funding_rate: fixed(wire.funding, FUNDING_RATE_SCALE)?,
        open_interest: fixed(wire.openInterest, NORMALIZED_QUANTITY_SCALE)?,
        day_volume: fixed(wire.dayNtlVlm, NOTIONAL_SCALE)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Re-encodes a fixture exactly as the socket delivers it: raw text the
    /// decoder parses without any intermediate `Value` float conversion.
    fn raw(value: &serde_json::Value) -> Box<serde_json::value::RawValue> {
        serde_json::value::RawValue::from_string(value.to_string()).expect("fixture encodes")
    }

    #[test]
    fn perp_context_decodes_all_fields_without_touching_candles() {
        let context = decode_asset_context(&raw(&json!({
            "markPx": "67000.5", "oraclePx": "67001.0", "midPx": "67000.75",
            "funding": "0.0001", "openInterest": "1234.5", "dayNtlVlm": "999.0",
        })))
        .expect("context");
        assert_eq!(context.mark_price, Some(6_700_050_000_000));
        assert_eq!(context.funding_rate, Some(1_000_000));
        assert_eq!(context.open_interest, Some(123_450_000_000));
    }

    #[test]
    fn live_precision_context_values_decode_at_their_scales() {
        // Observed live values: seven-decimal funding and ten-decimal
        // day-notional volume, plus numeric (non-string) wire forms.
        let context = decode_asset_context(&raw(&json!({
            "markPx": 67000.5, "oraclePx": "67001.0", "midPx": 67000.75,
            "funding": "0.0000125", "openInterest": 1234.5,
            "dayNtlVlm": "785361336.8372405767",
        })))
        .expect("live context");
        assert_eq!(context.mark_price, Some(6_700_050_000_000));
        assert_eq!(context.funding_rate, Some(125_000));
        assert_eq!(context.open_interest, Some(123_450_000_000));
        assert_eq!(context.day_volume, Some(7_853_613_368_372_405_767));
    }

    #[test]
    fn raw_wire_text_keeps_full_precision_without_floats() {
        // Written as literal wire text so no `f64` ever touches the value:
        // a 19-significant-digit notional stays exact to the unit.
        let wire = serde_json::value::RawValue::from_string(
            r#"{"dayNtlVlm": 785361336.8372405767, "funding": 0.0000125}"#.to_string(),
        )
        .expect("wire encodes");
        let context = decode_asset_context(&wire).expect("raw context");
        assert_eq!(context.day_volume, Some(7_853_613_368_372_405_767));
        assert_eq!(context.funding_rate, Some(125_000));
    }

    #[test]
    fn live_ctx_envelope_decodes_its_contents() {
        // Observed `activeAssetCtx` frame (public feed): the context rides
        // in `ctx`, with extra fields the decoder ignores.
        let wire = serde_json::value::RawValue::from_string(
            r#"{"coin":"BTC","ctx":{"funding":"0.0000125","openInterest":"35241.53662","prevDayPx":"79521.0","dayNtlVlm":"793093225.5723297596","oraclePx":"79916.0","markPx":"79884.0","midPx":"79884.5"}}"#.to_string(),
        )
        .expect("wire encodes");
        let context = decode_asset_context(&wire).expect("envelope decodes");
        assert_eq!(context.mark_price, Some(7_988_400_000_000));
        assert_eq!(context.oracle_price, Some(7_991_600_000_000));
        assert_eq!(context.mid_price, Some(7_988_450_000_000));
        assert_eq!(context.funding_rate, Some(125_000));
        assert_eq!(context.open_interest, Some(3_524_153_662_000));
        assert_eq!(context.day_volume, Some(7_930_932_255_723_297_596));
        // An envelope shape without usable `ctx` fails closed instead of
        // yielding an empty context that reads as valid.
        let missing = serde_json::value::RawValue::from_string(r#"{"coin":"BTC"}"#.to_string())
            .expect("wire encodes");
        assert!(decode_asset_context(&missing).is_err());
    }

    #[test]
    fn spot_context_leaves_funding_and_interest_absent() {
        let context = decode_asset_context(&raw(&json!({
            "markPx": "1.0", "midPx": "1.0",
        })))
        .expect("spot context");
        assert!(context.funding_rate.is_none());
        assert!(context.open_interest.is_none());
        assert_eq!(context.mark_price, Some(100_000_000));
    }

    #[test]
    fn malformed_context_fails_closed() {
        assert!(decode_asset_context(&raw(&json!({"markPx": "abc"}))).is_err());
        assert!(decode_asset_context(&raw(&json!({"markPx": true}))).is_err());
        assert!(decode_asset_context(&raw(&json!("nope"))).is_err());
    }
}
