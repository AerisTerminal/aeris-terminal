//! Checked decimal-to-fixed-point conversion without float round trips.

use aeris_market_data::expand_decimal_exponent;
pub use aeris_market_data::parse_decimal_to_fixed;

use crate::error::{CryptoError, CryptoResult};

/// Maximum supported fractional digits in one crypto wire decimal field.
pub const MAXIMUM_CRYPTO_DECIMALS: u32 = 18;

/// Normalized fixed-point scale for crypto prices (`10^8`).
pub const NORMALIZED_PRICE_SCALE: u32 = 8;
/// Normalized fixed-point scale for crypto quantities (`10^8`).
pub const NORMALIZED_QUANTITY_SCALE: u32 = 8;
/// Normalized fixed-point scale for funding rates (`10^10`).
pub const FUNDING_RATE_SCALE: u32 = 10;
/// Normalized fixed-point scale for notional volumes (`10^10`).
pub const NOTIONAL_SCALE: u32 = 10;

/// Parses an aggregate decimal into `value * 10^scale`, rounding away from zero
/// only when the wire carries more fractional digits than `scale`.
///
/// # Errors
/// Returns an error for malformed input, unsupported scale, or overflow.
pub fn parse_aggregate_decimal_to_fixed(raw: &str, scale: u32) -> CryptoResult<i64> {
    if scale > MAXIMUM_CRYPTO_DECIMALS {
        return Err(CryptoError::Decimal(
            "crypto decimal scale is unsupported".to_string(),
        ));
    }
    let text = raw.trim();
    if text.is_empty() {
        return Err(CryptoError::Decimal(
            "crypto decimal value is empty".to_string(),
        ));
    }
    let text = if text.bytes().any(|byte| byte == b'e' || byte == b'E') {
        expand_decimal_exponent(text).map_err(CryptoError::Decimal)?
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
        || fraction.len() > usize::try_from(MAXIMUM_CRYPTO_DECIMALS).unwrap_or(usize::MAX)
    {
        return Err(CryptoError::Decimal(
            "crypto decimal value is malformed".to_string(),
        ));
    }
    let scale_len = usize::try_from(scale)
        .map_err(|_| CryptoError::Decimal("crypto decimal scale is unsupported".to_string()))?;
    if fraction.len() <= scale_len {
        return parse_decimal_to_fixed(&text, scale).map_err(CryptoError::Decimal);
    }
    let retained = &fraction[..scale_len];
    let discarded = &fraction[scale_len..];
    let mut truncated = String::with_capacity(text.len());
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
    let value = parse_decimal_to_fixed(&truncated, scale).map_err(CryptoError::Decimal)?;
    if discarded
        .as_bytes()
        .first()
        .is_some_and(|digit| *digit >= b'5')
    {
        if negative {
            value
                .checked_sub(1)
                .ok_or_else(|| CryptoError::Decimal("crypto decimal value overflowed".to_string()))
        } else {
            value
                .checked_add(1)
                .ok_or_else(|| CryptoError::Decimal("crypto decimal value overflowed".to_string()))
        }
    } else {
        Ok(value)
    }
}

/// Multiplies two fixed-point values at `scale` and returns the result at `scale`,
/// rounding half away from zero via `i128` without float conversion.
///
/// Used for contract-size scaling (e.g. OKX `sz * ctVal` to base asset units).
///
/// # Errors
/// Returns an error when `scale > 18` or the result overflows `i64`.
pub fn multiply_fixed_decimal(lhs: i64, rhs: i64, scale: u32) -> CryptoResult<i64> {
    if scale > MAXIMUM_CRYPTO_DECIMALS {
        return Err(CryptoError::Decimal(
            "crypto decimal scale is unsupported".to_string(),
        ));
    }
    let divisor = 10_i128
        .checked_pow(scale)
        .ok_or_else(|| CryptoError::Decimal("crypto decimal scale overflowed".to_string()))?;
    let product = i128::from(lhs)
        .checked_mul(i128::from(rhs))
        .ok_or_else(|| {
            CryptoError::Decimal("crypto decimal multiplication overflowed".to_string())
        })?;
    let half = divisor / 2;
    let rounded =
        if product >= 0 {
            product.checked_add(half).ok_or_else(|| {
                CryptoError::Decimal("crypto decimal rounding overflowed".to_string())
            })? / divisor
        } else {
            product.checked_sub(half).ok_or_else(|| {
                CryptoError::Decimal("crypto decimal rounding overflowed".to_string())
            })? / divisor
        };
    i64::try_from(rounded).map_err(|_| {
        CryptoError::Decimal("crypto decimal multiplication overflowed i64".to_string())
    })
}

/// Infers the number of significant decimal places in a tick-size or step-size
/// string (e.g. `"0.0100"` -> `2`, `"0.0001"` -> `4`, `"1"` -> `0`).
#[must_use]
pub fn decimal_places_for_step(step: &str) -> u8 {
    let trimmed = step.trim();
    let expanded = if trimmed.bytes().any(|b| b == b'e' || b == b'E') {
        expand_decimal_exponent(trimmed).unwrap_or_else(|_| trimmed.to_string())
    } else {
        trimmed.to_string()
    };
    let Some((_, fraction)) = expanded.split_once('.') else {
        return 0;
    };
    let significant = fraction.trim_end_matches('0');
    u8::try_from(significant.len()).unwrap_or(8).min(8)
}

/// One JSON decimal captured as raw source text (string or number) without `f64`.
#[derive(Debug)]
pub struct RawDecimal(Box<serde_json::value::RawValue>);

impl RawDecimal {
    /// Returns the unquoted raw decimal text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let text = self.0.get().trim();
        text.strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(text)
    }

    /// Converts the captured decimal strictly into `value * 10^scale`.
    ///
    /// # Errors
    /// Returns an error for malformed text, over-precise fractions, or overflow.
    pub fn to_fixed(&self, scale: u32) -> CryptoResult<i64> {
        parse_decimal_to_fixed(self.as_str(), scale).map_err(CryptoError::Decimal)
    }

    /// Converts an aggregate decimal into `value * 10^scale`, rounding surplus precision.
    ///
    /// # Errors
    /// Returns an error for malformed input, unsupported scale, or overflow.
    pub fn to_fixed_aggregate(&self, scale: u32) -> CryptoResult<i64> {
        parse_aggregate_decimal_to_fixed(self.as_str(), scale)
    }
}

impl<'de> serde::Deserialize<'de> for RawDecimal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Box::<serde_json::value::RawValue>::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_decimal_rounds_half_away_from_zero() {
        assert_eq!(
            parse_aggregate_decimal_to_fixed("1.234567895", 8).unwrap(),
            123_456_790
        );
        assert_eq!(
            parse_aggregate_decimal_to_fixed("-1.234567895", 8).unwrap(),
            -123_456_790
        );
        assert_eq!(parse_aggregate_decimal_to_fixed("1e-7", 8).unwrap(), 10);
    }

    #[test]
    fn multiply_fixed_decimal_scales_contract_quantities_exactly() {
        // 15 contracts * 0.01 BTC/contract = 0.15 BTC at scale 8
        let contracts = parse_decimal_to_fixed("15", 8).unwrap();
        let ct_val = parse_decimal_to_fixed("0.01", 8).unwrap();
        assert_eq!(
            multiply_fixed_decimal(contracts, ct_val, 8).unwrap(),
            15_000_000
        );
    }

    #[test]
    fn decimal_places_for_step_trims_trailing_zeros() {
        assert_eq!(decimal_places_for_step("0.10"), 1);
        assert_eq!(decimal_places_for_step("0.00010000"), 4);
        assert_eq!(decimal_places_for_step("1.00"), 0);
        assert_eq!(decimal_places_for_step("1e-5"), 5);
    }
}
