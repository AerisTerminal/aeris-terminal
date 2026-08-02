//! Exact fixed-point parsing for provider price and size strings.
//!
//! Provider decimals are parsed into mantissa and scale without a float
//! round-trip: `1260.01` becomes mantissa 126001 at scale 2. Values are checked
//! against the mantissa bound and every parse is reversible to the exact source
//! decimal.

use crate::errors::CoinbaseError;

/// One exact decimal value as signed mantissa and decimal scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedPointValue {
    pub mantissa: i64,
    pub scale: u32,
}

/// Maximum accepted digits in the mantissa.
const MAXIMUM_DIGITS: usize = 18;

impl FixedPointValue {
    /// Parses a provider decimal string exactly.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or overflowing values.
    pub fn parse(source: &str) -> Result<Self, CoinbaseError> {
        let trimmed = source.trim();
        if trimmed.is_empty() || trimmed.len() > 40 {
            return Err(CoinbaseError::InvalidFixedPoint(source.to_string()));
        }
        let (negative, unsigned) = match trimmed.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, trimmed),
        };
        let mut mantissa: i64 = 0;
        let mut scale: u32 = 0;
        let mut seen_point = false;
        let mut seen_digit = false;
        let mut digits = 0_usize;
        for character in unsigned.chars() {
            match character {
                '0'..='9' => {
                    digits += 1;
                    if digits > MAXIMUM_DIGITS {
                        return Err(CoinbaseError::InvalidFixedPoint(source.to_string()));
                    }
                    mantissa = mantissa
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(i64::from(character as u8 - b'0')))
                        .ok_or_else(|| CoinbaseError::InvalidFixedPoint(source.to_string()))?;
                    if seen_point {
                        scale += 1;
                    }
                    seen_digit = true;
                }
                '.' if !seen_point => seen_point = true,
                _ => return Err(CoinbaseError::InvalidFixedPoint(source.to_string())),
            }
        }
        if !seen_digit {
            return Err(CoinbaseError::InvalidFixedPoint(source.to_string()));
        }
        Ok(Self {
            mantissa: if negative { -mantissa } else { mantissa },
            scale,
        })
    }

    /// Re-renders the exact decimal string.
    #[must_use]
    pub fn render(&self) -> String {
        let negative = self.mantissa < 0;
        let digits = self.mantissa.unsigned_abs().to_string();
        let padded = format!("{:0>width$}", digits, width = self.scale as usize + 1);
        let split = padded.len() - self.scale as usize;
        let mut output = String::new();
        if negative {
            output.push('-');
        }
        if self.scale == 0 {
            output.push_str(&padded);
        } else {
            output.push_str(&padded[..split]);
            output.push('.');
            output.push_str(&padded[split..]);
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::FixedPointValue;

    #[test]
    fn parses_exactly_without_float_rounding() {
        let value = FixedPointValue::parse("1260.01").expect("valid decimal");
        assert_eq!(value.mantissa, 126_001);
        assert_eq!(value.scale, 2);
        assert_eq!(value.render(), "1260.01");
        let size = FixedPointValue::parse("0.00000001").expect("valid decimal");
        assert_eq!(size.mantissa, 1);
        assert_eq!(size.scale, 8);
        assert_eq!(size.render(), "0.00000001");
    }

    #[test]
    fn rejects_malformed_and_overflowing_values() {
        assert!(FixedPointValue::parse("").is_err());
        assert!(FixedPointValue::parse("1.2.3").is_err());
        assert!(FixedPointValue::parse("abc").is_err());
        assert!(FixedPointValue::parse("9999999999999999999").is_err());
    }
}
