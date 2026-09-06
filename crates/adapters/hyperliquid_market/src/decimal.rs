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

/// Parses a decimal string directly into `value * 10^scale`.
///
/// Accepts an optional leading `-`, one optional `.`, and ASCII digits only.
///
/// # Errors
///
/// Returns an error for empty or malformed input, an unsupported scale,
/// more fractional digits than `scale` allows, or any overflowing
/// intermediate.
pub fn parse_decimal_to_fixed(raw: &str, scale: u32) -> Result<i64, String> {
    if scale > MAXIMUM_HYPERLIQUID_DECIMALS {
        return Err("hyperliquid decimal scale is unsupported".to_string());
    }
    let text = raw.trim();
    if text.is_empty() {
        return Err("hyperliquid decimal value is empty".to_string());
    }
    // Shortest-repr floats can surface exponent notation (e.g. `1e-7`);
    // normalize the decimal point first so the strict parser below still
    // sees plain decimal text.
    let text = if text.bytes().any(|byte| byte == b'e' || byte == b'E') {
        expand_exponent(text)?
    } else {
        text.to_string()
    };
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(&text)),
    };
    if digits.is_empty() {
        return Err("hyperliquid decimal value is malformed".to_string());
    }
    let mut parts = digits.split('.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return Err("hyperliquid decimal value is malformed".to_string());
    }
    if whole.is_empty() && fraction.is_empty() {
        return Err("hyperliquid decimal value is malformed".to_string());
    }
    for part in [whole, fraction] {
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("hyperliquid decimal value is malformed".to_string());
        }
    }
    if u32::try_from(fraction.len()).unwrap_or(u32::MAX) > scale {
        return Err("hyperliquid decimal precision exceeds scale".to_string());
    }
    let mut value: i64 = 0;
    for byte in whole.bytes() {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(byte - b'0')))
            .ok_or_else(|| "hyperliquid decimal value overflowed".to_string())?;
    }
    let missing = scale - u32::try_from(fraction.len()).unwrap_or(scale);
    for byte in fraction.bytes() {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(byte - b'0')))
            .ok_or_else(|| "hyperliquid decimal value overflowed".to_string())?;
    }
    value = ten_pow(missing)
        .and_then(|factor| value.checked_mul(factor))
        .ok_or_else(|| "hyperliquid decimal value overflowed".to_string())?;
    if negative {
        value = value
            .checked_neg()
            .ok_or_else(|| "hyperliquid decimal value overflowed".to_string())?;
    }
    Ok(value)
}

/// Expands one exponent-notation decimal (`1.25e-4`) into plain text.
///
/// Used only for shortest-repr JSON numbers; provider strings are expected
/// in plain form and pass through untouched.
fn expand_exponent(raw: &str) -> Result<String, String> {
    let malformed = || "hyperliquid decimal value is malformed".to_string();
    let (mantissa, exp_text) = raw.split_once(['e', 'E']).ok_or_else(malformed)?;
    let exp: i32 = exp_text.parse().map_err(|_| malformed())?;
    if exp.abs() > 36 {
        return Err(malformed());
    }
    let (negative, digits) = match mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, mantissa.strip_prefix('+').unwrap_or(mantissa)),
    };
    let mut parts = digits.split('.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(malformed());
    }
    let mut digits = format!("{whole}{fraction}");
    if digits.trim_matches('0').is_empty() {
        return Ok("0".to_string());
    }
    let mut point = i64::try_from(whole.len().max(1)).map_err(|_| malformed())? + i64::from(exp);
    // Strip leading zeroes; they shift the point with the digits.
    let leading = digits.bytes().take_while(|byte| *byte == b'0').count();
    digits.drain(..leading);
    point -= i64::try_from(leading).map_err(|_| malformed())?;
    let expanded = if point <= 0 {
        let zeros = usize::try_from(-point).map_err(|_| malformed())?;
        format!("0.{}{digits}", "0".repeat(zeros))
    } else {
        let at = usize::try_from(point).map_err(|_| malformed())?;
        if at >= digits.len() {
            format!("{digits}{}", "0".repeat(at - digits.len()))
        } else {
            let mut expanded = digits;
            expanded.insert(at, '.');
            expanded
        }
    };
    Ok(if negative {
        format!("-{expanded}")
    } else {
        expanded
    })
}

fn ten_pow(exp: u32) -> Option<i64> {
    let mut value: i64 = 1;
    for _ in 0..exp {
        value = value.checked_mul(10)?;
    }
    Some(value)
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
    fn decimal_parsing_rejects_malformed_and_overflowing_input() {
        for malformed in ["", "  ", ".", "-", "1.2.3", "abc", "0x10", "--1", "+-2"] {
            assert!(parse_decimal_to_fixed(malformed, 8).is_err(), "{malformed}");
        }
        assert!(parse_decimal_to_fixed("0.000000001", 8).is_err());
        assert!(parse_decimal_to_fixed("9223372036854775808", 0).is_err());
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
