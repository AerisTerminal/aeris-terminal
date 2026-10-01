//! Checked decimal to fixed-point conversion without float round trips.
//!
//! Hyperliquid prices and sizes arrive as decimal strings at market-specific
//! precision (spot `szDecimals`, perpetual significant-figure rules); the
//! WebSocket also emits some of the same fields as JSON numbers. Every
//! conversion below parses decimal text directly into a checked `i64`
//! fixed-point value at an explicit scale, so spot/perpetual precision
//! differences never collapse into one constant trading tick.
//!
//! Per the tick-and-lot-size rules, prices carry at most 5 significant
//! figures with at most `MAX_DECIMALS - szDecimals` decimal places, where
//! `MAX_DECIMALS` is 6 for perps and 8 for spot. The normalized scales below
//! cover the worst case of each field class:
//!
//! - prices (quotes, candles, marks): 8 places cover spot's `MAX_DECIMALS`;
//! - sizes (trade/level/candle quantities, open interest): 8 places cover
//!   the largest observed `szDecimals`;
//! - funding rates and notional volumes: 10 places cover live values such as
//!   funding `0.0000125` and day notional `785361336.8372405767`.

/// Maximum supported decimal places in one Hyperliquid decimal field.
pub const MAXIMUM_HYPERLIQUID_DECIMALS: u32 = 18;

/// Scale for normalized Hyperliquid fixed-point prices.
pub const NORMALIZED_PRICE_SCALE: u32 = 8;
/// Scale for normalized Hyperliquid fixed-point quantities.
pub const NORMALIZED_QUANTITY_SCALE: u32 = 8;
/// Scale for normalized Hyperliquid funding rates.
pub const FUNDING_RATE_SCALE: u32 = 10;
/// Scale for normalized Hyperliquid notional volumes.
pub const NOTIONAL_SCALE: u32 = 10;

use aeris_market_data::expand_decimal_exponent as expand_exponent;
pub use aeris_market_data::parse_decimal_to_fixed;

/// Parses a provider aggregate decimal into `value * 10^scale`, rounding only
/// when the wire carries more fractional precision than the canonical scale.
///
/// Hyperliquid aggregate candle volume can contain tiny extra decimal residue
/// (for example `939217.2893600001`) even though executable sizes use the
/// normalized quantity precision. Aggregate values are therefore quantized to
/// the nearest canonical unit, with exact half values rounded away from zero.
/// Trading prices and executable sizes continue to use the strict parser.
///
/// # Errors
///
/// Returns an error for malformed input, unsupported precision, or overflow.
pub fn parse_aggregate_decimal_to_fixed(raw: &str, scale: u32) -> Result<i64, String> {
    if scale > MAXIMUM_HYPERLIQUID_DECIMALS {
        return Err("hyperliquid decimal scale is unsupported".to_string());
    }
    let text = raw.trim();
    if text.is_empty() {
        return Err("hyperliquid decimal value is empty".to_string());
    }
    let text = if text.bytes().any(|byte| byte == b'e' || byte == b'E') {
        expand_exponent(text)?
    } else {
        text.to_string()
    };
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(&text)),
    };
    let mut parts = unsigned.split('.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > usize::try_from(MAXIMUM_HYPERLIQUID_DECIMALS).unwrap_or(usize::MAX)
    {
        return Err("hyperliquid decimal value is malformed".to_string());
    }
    let scale_len =
        usize::try_from(scale).map_err(|_| "hyperliquid decimal scale is unsupported")?;
    if fraction.len() <= scale_len {
        return parse_decimal_to_fixed(&text, scale);
    }
    let retained = &fraction[..scale_len];
    let discarded = &fraction[scale_len..];
    let mut truncated = String::new();
    if negative {
        truncated.push('-');
    }
    if whole.is_empty() {
        truncated.push('0');
    } else {
        truncated.push_str(whole);
    }
    if scale_len > 0 {
        truncated.push('.');
        truncated.push_str(retained);
    }
    let value = parse_decimal_to_fixed(&truncated, scale)?;
    if discarded
        .as_bytes()
        .first()
        .is_some_and(|digit| *digit >= b'5')
    {
        if negative {
            value
                .checked_sub(1)
                .ok_or_else(|| "hyperliquid decimal value overflowed".to_string())
        } else {
            value
                .checked_add(1)
                .ok_or_else(|| "hyperliquid decimal value overflowed".to_string())
        }
    } else {
        Ok(value)
    }
}
/// One JSON decimal captured as raw source text.
///
/// `serde_json` without `arbitrary_precision` parses fractional JSON numbers
/// into `f64`, which cannot preserve the provider's original decimal text
/// (a 19-significant-digit notional such as `785361336.8372405767` already
/// lost units before any conversion runs). Capturing the raw text before any
/// float conversion keeps every decimal exact, whether the wire form is a
/// string or a number.
#[derive(Debug)]
pub struct RawDecimal(Box<serde_json::value::RawValue>);

impl RawDecimal {
    /// Converts the captured decimal into `value * 10^scale`.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed text, over-precise fractions, or
    /// overflowing intermediates.
    pub fn to_fixed(&self, scale: u32) -> Result<i64, String> {
        let text = self.0.get().trim();
        let text = text
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(text);
        parse_decimal_to_fixed(text, scale)
    }
    /// Converts an aggregate provider decimal into the canonical fixed scale,
    /// quantizing only surplus fractional precision.
    ///
    /// # Errors
    /// Returns an error for malformed input, unsupported precision, or overflow.
    pub fn to_fixed_aggregate(&self, scale: u32) -> Result<i64, String> {
        let text = self.0.get().trim();
        let text = text
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(text);
        parse_aggregate_decimal_to_fixed(text, scale)
    }
}

impl<'de> serde::Deserialize<'de> for RawDecimal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = Box::<serde_json::value::RawValue>::deserialize(deserializer)?;
        // Only strings and numbers are decimals; anything else (bools,
        // null, objects, arrays) fails here rather than at conversion.
        let text = raw.get().trim_start();
        let numeric = text
            .as_bytes()
            .first()
            .is_some_and(|byte| *byte == b'"' || *byte == b'-' || byte.is_ascii_digit());
        if numeric {
            Ok(Self(raw))
        } else {
            Err(serde::de::Error::custom(
                "hyperliquid decimal value is malformed",
            ))
        }
    }
}

/// Returns the normalized price scale for a market class.
///
/// The scale is intentionally constant across markets so one canonical
/// fixed-point domain carries every market; per-market significant-figure
/// rules are enforced by rejecting over-precise input, not by varying the
/// stored scale.
#[must_use]
pub fn scale_for_market(_sz_decimals: u32) -> u32 {
    NORMALIZED_PRICE_SCALE
}

/// Returns the valid price increment near `reference_price`, in units of
/// [`NORMALIZED_PRICE_SCALE`].
///
/// Hyperliquid has no fixed tick: prices carry at most 5 significant figures
/// and at most `MAX_DECIMALS - szDecimals` decimal places (`MAX_DECIMALS` is 6
/// for perps and 8 for spot), and integer prices are always valid. The
/// increment is therefore exact only for prices with the same number of
/// integer digits as the reference.
///
/// # Errors
///
/// Returns an error when the reference price is malformed, over-precise, or
/// not positive.
pub fn price_increment_near(
    reference_price: &str,
    sz_decimals: u32,
    spot: bool,
) -> Result<i64, String> {
    const SIGNIFICANT_FIGURES: u32 = 5;
    let price = parse_decimal_to_fixed(reference_price, NORMALIZED_PRICE_SCALE)?;
    if price <= 0 {
        return Err("hyperliquid reference price must be positive".to_string());
    }
    let digits = price.unsigned_abs().ilog10() + 1;
    // Decimal places the significant-figure rule leaves after the leading
    // digit; integer prices (5+ integer digits) collapse to zero places.
    let significant_places = (NORMALIZED_PRICE_SCALE + SIGNIFICANT_FIGURES).saturating_sub(digits);
    let maximum_places = if spot { 8_u32 } else { 6 }.saturating_sub(sz_decimals);
    let places = significant_places
        .min(maximum_places)
        .min(NORMALIZED_PRICE_SCALE);
    Ok(10_i64.pow(NORMALIZED_PRICE_SCALE - places))
}

/// Parses a size string at the normalized quantity scale.
///
/// # Errors
///
/// Returns an error for malformed input, over-precise fractions, or
/// overflowing intermediates.
pub fn quantity_for_size(raw: &str) -> Result<i64, String> {
    parse_decimal_to_fixed(raw, NORMALIZED_QUANTITY_SCALE)
}

/// Parses a funding-rate decimal at the funding scale.
///
/// # Errors
///
/// Returns an error for malformed input, over-precise fractions, or
/// overflowing intermediates.
pub fn funding_for_rate(raw: &str) -> Result<i64, String> {
    parse_decimal_to_fixed(raw, FUNDING_RATE_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_parsing_never_touches_float() {
        assert_eq!(parse_decimal_to_fixed("0", 8), Ok(0));
        assert_eq!(parse_decimal_to_fixed("1.5", 8), Ok(150_000_000));
        assert_eq!(parse_decimal_to_fixed("0.00000001", 8), Ok(1));
        assert_eq!(parse_decimal_to_fixed("-2.25", 2), Ok(-225));
        assert_eq!(parse_decimal_to_fixed("  10.00  ", 2), Ok(1000));
    }

    #[test]
    fn price_increment_follows_the_documented_tick_rules() {
        // Documented examples: perp `1234.5` valid, `1234.56` invalid.
        assert_eq!(price_increment_near("1234.5", 0, false), Ok(10_000_000));
        // Perp `0.001234` valid, `0.0012345` invalid (6-decimal cap).
        assert_eq!(price_increment_near("0.001234", 0, false), Ok(100));
        // Perp with szDecimals 1: `0.01234` valid, `0.012345` invalid.
        assert_eq!(price_increment_near("0.01234", 1, false), Ok(1_000));
        // Spot keeps 8 - szDecimals places: `0.0001234` valid at szDecimals 1.
        assert_eq!(price_increment_near("0.0001234", 1, true), Ok(10));
        // BTC perp (szDecimals 5) near 85k and above 100k: integer prices.
        assert_eq!(price_increment_near("85031.0", 5, false), Ok(100_000_000));
        assert_eq!(price_increment_near("123456", 5, false), Ok(100_000_000));
        // ETH perp (szDecimals 4) near 3k: one decimal place.
        assert_eq!(price_increment_near("3012.4", 4, false), Ok(10_000_000));
        assert!(price_increment_near("0", 0, false).is_err());
        assert!(price_increment_near("-1", 0, false).is_err());
        assert!(price_increment_near("abc", 0, false).is_err());
    }

    #[test]
    fn decimal_parsing_rejects_malformed_and_overflowing_input() {
        for malformed in ["", "  ", ".", "-", "1.2.3", "abc", "0x10", "--1", "+-2"] {
            assert!(parse_decimal_to_fixed(malformed, 8).is_err(), "{malformed}");
        }
        assert!(parse_decimal_to_fixed("0.000000001", 8).is_err());
        assert!(parse_decimal_to_fixed("9223372036854775808", 0).is_err());
    }

    #[test]
    fn aggregate_decimal_quantization_is_explicit_and_deterministic() {
        assert_eq!(
            parse_aggregate_decimal_to_fixed("939217.2893600001", 8),
            Ok(93_921_728_936_000)
        );
        assert_eq!(
            parse_aggregate_decimal_to_fixed("1.000000005", 8),
            Ok(100_000_001)
        );
        assert_eq!(
            parse_aggregate_decimal_to_fixed("-1.000000005", 8),
            Ok(-100_000_001)
        );
        assert_eq!(
            parse_aggregate_decimal_to_fixed("1.000000004", 8),
            Ok(100_000_000)
        );
        // Strict executable-value conversion remains fail-closed.
        assert!(parse_decimal_to_fixed("1.000000005", 8).is_err());
    }
    #[test]
    fn live_precision_values_land_in_their_explicit_scales() {
        // Live funding rate with seven decimals decodes at funding scale.
        assert_eq!(funding_for_rate("0.0000125"), Ok(125_000));
        // Nine-decimal precision exceeds the price scale but fits funding.
        assert!(parse_decimal_to_fixed("0.000000125", NORMALIZED_PRICE_SCALE).is_err());
        assert_eq!(funding_for_rate("0.000000125"), Ok(1_250));
        // Live day-notional volume with ten decimals needs the notional scale.
        assert_eq!(
            parse_decimal_to_fixed("785361336.8372405767", NOTIONAL_SCALE),
            Ok(7_853_613_368_372_405_767)
        );
        // Spot prices may carry up to eight decimals (MAX_DECIMALS for spot).
        assert_eq!(
            parse_decimal_to_fixed("0.00012345", NORMALIZED_PRICE_SCALE),
            Ok(12_345)
        );
        // Five-significant-figure perpetual ticks keep exact values.
        assert_eq!(
            parse_decimal_to_fixed("67432.12345", NORMALIZED_PRICE_SCALE),
            Ok(6_743_212_345_000)
        );
        assert_eq!(quantity_for_size("0.00000001"), Ok(1));
    }

    #[test]
    fn raw_decimals_keep_provider_text_exact() {
        // A 19-significant-digit notional loses units the moment it passes
        // through `f64`; captured raw it stays exact.
        let raw: RawDecimal = serde_json::from_str("\"785361336.8372405767\"").expect("raw string");
        assert_eq!(raw.to_fixed(NOTIONAL_SCALE), Ok(7_853_613_368_372_405_767));
        let raw: RawDecimal = serde_json::from_str("29258.0").expect("raw number");
        assert_eq!(raw.to_fixed(NORMALIZED_PRICE_SCALE), Ok(2_925_800_000_000));
        let raw: RawDecimal = serde_json::from_str("189").expect("raw integer");
        assert_eq!(raw.to_fixed(NORMALIZED_QUANTITY_SCALE), Ok(18_900_000_000));
        let raw: RawDecimal = serde_json::from_str("0.0000125").expect("raw rate");
        assert_eq!(raw.to_fixed(FUNDING_RATE_SCALE), Ok(125_000));
        let raw: RawDecimal = serde_json::from_str("\"67000.5\"").expect("raw px");
        assert_eq!(raw.to_fixed(NORMALIZED_PRICE_SCALE), Ok(6_700_050_000_000));
        assert!(serde_json::from_str::<RawDecimal>("true").is_err());
        assert!(serde_json::from_str::<RawDecimal>("null").is_err());
        assert!(serde_json::from_str::<RawDecimal>("{}").is_err());
    }
}
